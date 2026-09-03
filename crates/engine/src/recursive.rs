//! Reading a `WITH RECURSIVE` CTE: which branch seeds it, which branch
//! repeats, and the shapes 8.3 refuses.
//!
//! Everything here is about the statement rather than the rows.
//! `Engine::recursive_cte` runs what [`plan`] produces; this module decides
//! what there is to run and raises MySQL's own errors where there is nothing.
//!
//! The rules below were measured against `mysql:8.3` rather than read, because
//! several of them are not what the manual's summary suggests. The two that
//! caught this out: `ORDER BY` over the CTE's UNION is refused while `LIMIT`
//! over the same UNION is *accepted* and stops the recursion early, and a
//! recursive branch may be joined to the anchor with `UNION ALL` and then to a
//! later branch with `UNION` — the mixture is refused in one direction only.

use sqlparser::ast::{
    Cte, Distinct, Expr, GroupByExpr, JoinOperator, Query, Select, SelectItem, SetExpr,
    SetOperator, SetQuantifier, TableFactor, TableWithJoins,
};

use crate::error::{EngineError, EngineResult};

/// A recursive CTE split into the part that runs once and the part that
/// repeats.
pub(crate) struct Plan {
    /// The branches that do not name the CTE, joined as they were written.
    ///
    /// Run once, and the *only* thing the CTE's columns are typed from — which
    /// is why it is kept whole rather than as a row set. Measured on 8.3:
    /// `WITH RECURSIVE t AS (SELECT 'a' AS x UNION ALL SELECT CONCAT(x, 'b')
    /// FROM t ...)` is 1406 there, because `x` is the anchor's `varchar(1)`
    /// and the second round does not fit it. `Engine::recursive_cte` writes
    /// every round through the anchor's types for that reason, and its own
    /// note records the two ways `SqlType::coerce` falls short of doing it.
    pub anchor: SetExpr,
    /// The branches that do name it, joined as they were written. Run once per
    /// iteration, against the rows the previous iteration added.
    pub step: SetExpr,
    /// Whether a produced row is dropped when the CTE already holds it.
    ///
    /// This is the whole difference between `UNION` and `UNION ALL` here, and
    /// it is not a difference of presentation: over a cycle in the data —
    /// A the parent of B and B the parent of A — `UNION` terminates and
    /// `UNION ALL` runs to `cte_max_recursion_depth` and is aborted with 3636.
    pub distinct: bool,
}

/// Splits `cte` into its anchor and its recursive part, or `None` when it does
/// not name itself and is therefore an ordinary CTE.
///
/// `WITH RECURSIVE` says a CTE *may* recurse, not that it does: 8.3 answers
/// `WITH RECURSIVE t AS (SELECT 1) SELECT * FROM t` and gives the column the
/// `NOT NULL` a one-branch CTE has, where a recursive one's columns are all
/// nullable.
pub(crate) fn plan(cte: &Cte, name: &str) -> EngineResult<Option<Plan>> {
    let mut branches = Vec::new();
    let mut joins = Vec::new();
    flatten(&cte.query.body, &mut branches, &mut joins);

    let recursive: Vec<bool> = branches.iter().map(|b| names_cte(b, name)).collect();
    let Some(split) = recursive.iter().position(|&r| r) else {
        return Ok(None);
    };

    // "one or more non-recursive query blocks followed by one or more
    // recursive ones", which fails in both directions: a recursive branch
    // first leaves nothing to seed the CTE, and a non-recursive branch after
    // one has no iteration to be part of.
    if split == 0 || recursive[split..].iter().any(|&r| !r) {
        return Err(EngineError::new(
            3574,
            "HY000",
            format!(
                "Recursive Common Table Expression '{name}' should have one or more \
                 non-recursive query blocks followed by one or more recursive ones"
            ),
        ));
    }

    // UNION and nothing else. INTERSECT and EXCEPT have no iteration to give:
    // a branch cannot both be a fixpoint and remove rows the last one added.
    if joins
        .iter()
        .any(|(op, _)| !matches!(op, SetOperator::Union))
    {
        return Err(EngineError::new(
            3573,
            "HY000",
            format!("Recursive Common Table Expression '{name}' should contain a UNION"),
        ));
    }

    // 8.3 refuses `UNION` followed by `UNION ALL` and accepts the mixture the
    // other way round, so this is asked as "a DISTINCT with an ALL after it"
    // rather than as "the two are mixed".
    let distinct_at = joins
        .iter()
        .position(|(_, q)| !is_all(q))
        .unwrap_or(joins.len());
    if joins[distinct_at..].iter().any(|(_, q)| is_all(q)) {
        return Err(not_supported_yet(
            "recursive query blocks with UNION DISTINCT then UNION ALL, \
             in recursive Common Table Expression",
        ));
    }

    // The CTE's own ORDER BY, which sorts a result that does not exist until
    // the recursion has finished. Its LIMIT is accepted and handled by the
    // caller, which is measured and not symmetrical.
    if cte.query.order_by.is_some() {
        return Err(not_supported_yet(
            "ORDER BY over UNION in recursive Common Table Expression",
        ));
    }

    for branch in &branches[split..] {
        check_branch(branch, name)?;
    }

    Ok(Some(Plan {
        anchor: joined(&branches[..split], &joins[..split.saturating_sub(1)]),
        step: joined(&branches[split..], &joins[split..]),
        // Anywhere in the chain, which is measured: `SELECT 1 UNION ALL SELECT
        // 1 UNION SELECT n + 1 FROM t WHERE n < 3` answers 1, 2, 3 on 8.3 and
        // not 1, 1, 2, 3, so the one DISTINCT de-duplicates the anchor rows as
        // well as the iterations.
        distinct: distinct_at < joins.len(),
    }))
}

/// Which entries of a WITH list anything could reach, one flag per CTE in
/// declaration order.
///
/// 8.3 does not look at a recursive CTE nothing reads at all. `WITH RECURSIVE
/// unused AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM unused) SELECT 42`
/// answers 42, where running it would be 3636 after a thousand iterations —
/// and so does the same statement over a CTE whose recursive branch is
/// written *first*, where resolving it would be 3574. So `materialise_ctes`
/// skips one nothing names whole, rather than planning it and refusing.
///
/// Reachability is transitive, and that is measured too: in `WITH RECURSIVE
/// a AS (<malformed>), b AS (SELECT n FROM a) SELECT 42`, `b` names `a` and
/// the body names neither, so 8.3 answers 42 rather than refusing `a`. A
/// single pass over "does any *other* CTE mention it" got that wrong, because
/// it counted the mention in a sibling nothing reads either. So the reachable
/// set grows from the body outwards to a fixpoint.
///
/// Asked of the *rendered* statement rather than of its shape, and
/// deliberately: a structural walk that misses a variant answers "not
/// referenced" for a CTE that is, and the rows nothing produced would then be
/// a wrong answer rather than an error. A textual scan errs the other way —
/// the name as a column, as a string literal or as another CTE's alias all
/// count, and the cost of a false match is a CTE evaluated that need not have
/// been.
pub(crate) fn read_ctes(query: &Query) -> Vec<bool> {
    let Some(with) = &query.with else {
        return Vec::new();
    };
    let names: Vec<&str> = with
        .cte_tables
        .iter()
        .map(|cte| cte.alias.name.value.as_str())
        .collect();
    // The *definition* of each, not the whole entry: an entry renders its own
    // alias too, which would make every CTE look like a reader of itself.
    let definitions: Vec<String> = with
        .cte_tables
        .iter()
        .map(|cte| cte.query.to_string())
        .collect();

    // Separated by spaces, because the join is what the identifier boundary
    // below is read against: run together, `SELECT id FROM FT` and `ORDER BY
    // id` spell `ft` inside `ftorder`, and the CTE the statement plainly reads
    // looked unread.
    let mut outer = format!(" {} ", query.body);
    if let Some(order_by) = &query.order_by {
        outer.push_str(&format!("{order_by} "));
    }
    if let Some(limit) = &query.limit_clause {
        outer.push_str(&format!("{limit} "));
    }

    let mut read: Vec<bool> = names
        .iter()
        .map(|name| names_identifier(&outer, name))
        .collect();
    // To a fixpoint. Bounded by the number of CTEs, because each pass that
    // changes anything sets at least one more flag and none is ever cleared.
    loop {
        let mut grew = false;
        for reader in 0..names.len() {
            if !read[reader] {
                continue;
            }
            for (target, name) in names.iter().enumerate() {
                if target == reader || read[target] {
                    continue;
                }
                if names_identifier(&definitions[reader], name) {
                    read[target] = true;
                    grew = true;
                }
            }
        }
        if !grew {
            return read;
        }
    }
}

/// Whether `text` contains `name` as a whole identifier.
///
/// Case-insensitively, which is the over-matching direction and the safe one:
/// a name this misses is a CTE skipped that something reads, which answers
/// 1146 for a statement MySQL answers. The neighbours decide the boundary: `t`
/// must not match the `t` in `parentId`, or a CTE with a one-letter name would
/// look read by every statement.
fn names_identifier(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let lowered = text.to_lowercase();
    let needle = name.to_lowercase();
    let mut from = 0;
    while let Some(at) = lowered[from..].find(&needle) {
        let start = from + at;
        let end = start + needle.len();
        let before = lowered[..start].chars().next_back();
        let after = lowered[end..].chars().next();
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            return true;
        }
        // Past this occurrence's first character, so an overlapping one is
        // still found.
        from = start + lowered[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// What 8.3 refuses inside a recursive query block, in the order it refuses
/// it.
///
/// The order is measured rather than chosen: `SELECT DISTINCT MAX(n) FROM t`
/// is 1235 and `SELECT DISTINCT a.n FROM t a JOIN t b ON a.n = b.n` is 3577,
/// so the reference rule is asked before the clause rule and the clause rule
/// before the aggregation rule.
fn check_branch(branch: &SetExpr, name: &str) -> EngineResult<()> {
    let seen = sightings(branch, name);

    if seen.nested || seen.in_from > 1 {
        return Err(EngineError::new(
            3577,
            "HY000",
            format!(
                "In recursive query block of Recursive Common Table Expression '{name}', \
                 the recursive table must be referenced only once, and not in any subquery"
            ),
        ));
    }

    if clipped(branch) {
        return Err(not_supported_yet(
            "ORDER BY / LIMIT / SELECT DISTINCT in recursive query block of \
             Common Table Expression",
        ));
    }

    if grouped(branch) {
        return Err(EngineError::new(
            3575,
            "HY000",
            format!(
                "Recursive Common Table Expression '{name}' can contain neither aggregation \
                 nor window functions in recursive query block"
            ),
        ));
    }

    if seen.padded_by_outer_join {
        return Err(EngineError::new(
            3576,
            "HY000",
            format!(
                "In recursive query block of Recursive Common Table Expression '{name}', \
                 the recursive table must neither be in the right argument of a LEFT JOIN, \
                 nor be forced to be non-first with join order hints"
            ),
        ));
    }

    Ok(())
}

/// One of 8.3's own `ER_NOT_SUPPORTED_YET` refusals, spelled the way it spells
/// them.
///
/// Not [`crate::error::unsupported`], which says *this engine* has a gap and
/// carries `0A000`. These three are MySQL refusing a statement it defines and
/// does not implement, with SQLSTATE 42000 — reproducing the refusal is the
/// compatible answer, and calling it an ultralight gap would tell a caller to
/// go and run it somewhere real, where it fails the same way.
fn not_supported_yet(what: &str) -> EngineError {
    EngineError::new(
        1235,
        "42000",
        format!("This version of MySQL doesn't yet support '{what}'"),
    )
}

/// The same two variants `Engine::set_expr` reads as ALL, so a recursive CTE
/// and a plain UNION cannot disagree about which quantifier keeps duplicates.
fn is_all(quantifier: &SetQuantifier) -> bool {
    matches!(quantifier, SetQuantifier::All | SetQuantifier::AllByName)
}

/// The branches of a chain of set operations, left to right, with the operator
/// that joined each to the one before it.
///
/// `joins[i]` joins `branches[i]` to `branches[i + 1]`, which is what lets a
/// slice of branches be rebuilt with the operators it was written with.
/// Parentheses stop the walk: `(SELECT 1 UNION SELECT 2) UNION SELECT 3` is
/// two branches here, and the bracketed one is a branch in its own right for
/// every rule below — which is how 8.3 reads it too, since a bracketed branch
/// may carry the `LIMIT` an unbracketed one may not.
fn flatten<'a>(
    body: &'a SetExpr,
    branches: &mut Vec<&'a SetExpr>,
    joins: &mut Vec<(SetOperator, SetQuantifier)>,
) {
    match body {
        SetExpr::SetOperation {
            left,
            op,
            set_quantifier,
            right,
        } => {
            flatten(left, branches, joins);
            joins.push((*op, *set_quantifier));
            branches.push(right);
        }
        single => branches.push(single),
    }
}

/// Rebuilds a run of branches into the set operation they were written as.
///
/// Left-associative, matching the parse, so the rebuilt tree evaluates the way
/// the original did.
fn joined(branches: &[&SetExpr], joins: &[(SetOperator, SetQuantifier)]) -> SetExpr {
    let mut combined = branches[0].clone();
    for (branch, (op, quantifier)) in branches[1..].iter().zip(joins) {
        combined = SetExpr::SetOperation {
            left: Box::new(combined),
            op: *op,
            set_quantifier: *quantifier,
            right: Box::new((*branch).clone()),
        };
    }
    combined
}

/// Where a branch names the CTE.
///
/// Three facts and not one, because 8.3 gives each of them a different error
/// and the counts have to be kept apart to tell them apart.
#[derive(Default)]
struct Sightings {
    /// In the branch's own FROM clause, which is the one place 8.3 allows —
    /// and allows exactly once.
    in_from: usize,
    /// Anywhere a row of the CTE would have to be read more than once to
    /// answer: a derived table, or a subquery in any clause.
    nested: bool,
    /// On the side of an outer join that can be padded, where an unmatched row
    /// would invent a CTE row that no round produced.
    padded_by_outer_join: bool,
}

impl Sightings {
    /// Whether the CTE was named at all, wherever it was.
    fn any(&self) -> bool {
        self.in_from > 0 || self.nested
    }
}

fn names_cte(branch: &SetExpr, name: &str) -> bool {
    sightings(branch, name).any()
}

fn sightings(branch: &SetExpr, name: &str) -> Sightings {
    let mut seen = Sightings::default();
    walk_set_expr(branch, name, &mut seen);
    seen
}

fn walk_set_expr(branch: &SetExpr, name: &str, seen: &mut Sightings) {
    match branch {
        SetExpr::Select(select) => walk_select(select, name, seen),
        // A bracketed branch. Its own WITH may shadow the name, which
        // `walk_query` checks; everything else inside is read as it would be
        // unbracketed.
        SetExpr::Query(query) => walk_query(query, name, seen),
        SetExpr::SetOperation { left, right, .. } => {
            walk_set_expr(left, name, seen);
            walk_set_expr(right, name, seen);
        }
        // VALUES, INSERT, UPDATE, DELETE and TABLE. None of them can carry a
        // FROM clause naming the CTE, and a subquery inside one would be
        // `nested` — which is 3577 either way, so nothing here changes the
        // answer.
        _ => {}
    }
}

fn walk_select(select: &Select, name: &str, seen: &mut Sightings) {
    for from in &select.from {
        walk_from(from, name, seen);
    }
    for item in &select.projection {
        match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                walk_expr(e, name, seen)
            }
            _ => {}
        }
    }
    for e in select
        .selection
        .iter()
        .chain(select.having.iter())
        .chain(select.qualify.iter())
    {
        walk_expr(e, name, seen);
    }
    if let GroupByExpr::Expressions(exprs, _) = &select.group_by {
        for e in exprs {
            walk_expr(e, name, seen);
        }
    }
}

fn walk_from(from: &TableWithJoins, name: &str, seen: &mut Sightings) {
    // Each side into its own tally, because which *side* of a join the
    // reference is on is a question a merged one can no longer answer. `left`
    // accumulates everything joined so far, which is what the next join's left
    // argument is.
    let mut left = Sightings::default();
    walk_factor(&from.relation, name, &mut left);
    for join in &from.joins {
        let mut right = Sightings::default();
        walk_factor(&join.relation, name, &mut right);
        // 8.3 refuses the recursive table wherever an outer join could invent
        // a row for it, and that is both sides depending on the operator: a
        // RIGHT JOIN is rewritten into a LEFT JOIN with the operands swapped,
        // so `FROM t RIGHT JOIN z ON ...` is the same 3576 as
        // `FROM z LEFT JOIN t ON ...`. Measured, both.
        let padded = match join.join_operator {
            JoinOperator::Left(_) | JoinOperator::LeftOuter(_) => right.any(),
            JoinOperator::Right(_) | JoinOperator::RightOuter(_) => left.any(),
            _ => false,
        };
        left.padded_by_outer_join |= padded | right.padded_by_outer_join;
        left.in_from += right.in_from;
        left.nested |= right.nested;
        for e in join_constraint_expressions(&join.join_operator) {
            walk_expr(e, name, &mut left);
        }
    }
    seen.in_from += left.in_from;
    seen.nested |= left.nested;
    seen.padded_by_outer_join |= left.padded_by_outer_join;
}

fn join_constraint_expressions(operator: &JoinOperator) -> Vec<&Expr> {
    use sqlparser::ast::JoinConstraint;
    let constraint = match operator {
        JoinOperator::Join(c)
        | JoinOperator::Inner(c)
        | JoinOperator::Left(c)
        | JoinOperator::LeftOuter(c)
        | JoinOperator::Right(c)
        | JoinOperator::RightOuter(c)
        | JoinOperator::FullOuter(c)
        | JoinOperator::Semi(c)
        | JoinOperator::LeftSemi(c)
        | JoinOperator::RightSemi(c)
        | JoinOperator::Anti(c)
        | JoinOperator::LeftAnti(c)
        | JoinOperator::RightAnti(c)
        | JoinOperator::CrossJoin(c)
        | JoinOperator::StraightJoin(c) => c,
        _ => return Vec::new(),
    };
    match constraint {
        JoinConstraint::On(e) => vec![e],
        _ => Vec::new(),
    }
}

fn walk_factor(factor: &TableFactor, name: &str, seen: &mut Sightings) {
    match factor {
        // Unqualified only: a CTE is in no database, and `probe.t` names a
        // table however the CTE is spelled.
        //
        // Spelled exactly, which is measured and is the opposite of what
        // `Engine::relation_of` does for a name already in scope. A CTE name
        // is a table name, and the differential's MySQL runs on Linux with
        // `lower_case_table_names = 0`: `WITH RECURSIVE t AS (... FROM T ...)`
        // is 1146 there rather than a recursion. Case-insensitively, the
        // PascalCase shape this whole feature exists for broke — `WITH
        // RECURSIVE folder AS (SELECT id FROM Folder ...)` read its own name
        // in the *table* it walks and was refused with 3574.
        TableFactor::Table { name: written, .. } => {
            if written.0.len() == 1 && crate::name::dotted(written) == name {
                seen.in_from += 1;
            }
        }
        TableFactor::Derived { subquery, .. } => {
            let mut inside = Sightings::default();
            walk_query(subquery, name, &mut inside);
            if inside.any() {
                seen.nested = true;
            }
        }
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => walk_from(table_with_joins, name, seen),
        _ => {}
    }
}

fn walk_query(query: &Query, name: &str, seen: &mut Sightings) {
    // A nested WITH declaring the same name shadows this CTE, so a reference
    // inside is to that one and not to this.
    if query.with.iter().any(|with| {
        with.cte_tables
            .iter()
            .any(|inner| inner.alias.name.value.eq_ignore_ascii_case(name))
    }) {
        return;
    }
    walk_set_expr(&query.body, name, seen);
}

/// A reference inside an expression is always [`Sightings::nested`]: it can
/// only be a subquery, which 8.3 refuses whatever else is true of the branch.
fn walk_expr(e: &Expr, name: &str, seen: &mut Sightings) {
    let queries: Vec<&Query> = match e {
        Expr::Subquery(query) => vec![query],
        Expr::InSubquery { subquery, .. } => vec![subquery],
        Expr::Exists { subquery, .. } => vec![subquery],
        _ => Vec::new(),
    };
    for query in queries {
        let mut inside = Sightings::default();
        walk_query(query, name, &mut inside);
        if inside.any() {
            seen.nested = true;
        }
    }
    if let Expr::Function(f) = e {
        for argument in crate::exec::function_arguments(f) {
            walk_expr(&argument, name, seen);
        }
    }
    for sub in crate::exec::subexpressions(e) {
        walk_expr(sub, name, seen);
    }
}

/// Whether a branch sorts, limits or de-duplicates its own rows, which 8.3
/// refuses in a recursive one because there is no whole result yet to do it
/// over.
fn clipped(branch: &SetExpr) -> bool {
    match branch {
        SetExpr::Select(select) => matches!(
            select.distinct,
            Some(Distinct::Distinct) | Some(Distinct::On(_))
        ),
        SetExpr::Query(query) => {
            query.order_by.is_some() || query.limit_clause.is_some() || clipped(&query.body)
        }
        SetExpr::SetOperation { left, right, .. } => clipped(left) || clipped(right),
        _ => false,
    }
}

/// Whether a branch aggregates or windows, which 8.3 refuses in a recursive
/// one: both read the whole of a result the iteration has not finished
/// producing.
fn grouped(branch: &SetExpr) -> bool {
    match branch {
        SetExpr::Select(select) => {
            !matches!(&select.group_by, GroupByExpr::Expressions(exprs, _) if exprs.is_empty())
                || select.having.is_some()
                || select
                    .projection
                    .iter()
                    .any(crate::exec::select_item_aggregates)
                || select.projection.iter().any(select_item_windows)
        }
        SetExpr::Query(query) => grouped(&query.body),
        SetExpr::SetOperation { left, right, .. } => grouped(left) || grouped(right),
        _ => false,
    }
}

fn select_item_windows(item: &SelectItem) -> bool {
    match item {
        SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
            contains_window(e)
        }
        _ => false,
    }
}

fn contains_window(e: &Expr) -> bool {
    match e {
        Expr::Function(f) => {
            f.over.is_some()
                || crate::exec::function_arguments(f)
                    .iter()
                    .any(contains_window)
        }
        other => crate::exec::subexpressions(other)
            .into_iter()
            .any(contains_window),
    }
}
