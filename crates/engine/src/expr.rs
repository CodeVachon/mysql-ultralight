//! Expression evaluation.

use sqlparser::ast::{
    BinaryOperator, CaseWhen, DateTimeField, Expr, FunctionArg, FunctionArgExpr, FunctionArguments,
    Query, SelectItem, SetExpr, UnaryOperator, Value as AstValue, ValueWithSpan,
};

use crate::error::{unsupported, EngineError, EngineResult};
use crate::functions;
use crate::session::Session;
use crate::value::Value;

/// One column visible to an expression.
#[derive(Debug, Clone)]
pub struct ColumnRef {
    /// The database a `db.table.column` qualifier must name to reach this
    /// column, empty where the FROM item it came from is in none.
    ///
    /// Empty means the database part is *ignored* rather than refused, which
    /// is 8.3's answer for a FROM item that has no database of its own — a
    /// derived table, a CTE, a `JSON_TABLE`. Measured: `SELECT nodb.d.b FROM
    /// (SELECT b FROM J2) d` answers there. A qualified *wildcard* over the
    /// same item is 1051 whatever database it names, so that rule lives on
    /// `Relation` rather than here.
    pub database: String,
    /// Table name or alias the column is reachable through, empty when none.
    pub table: String,
    pub name: String,
}

impl ColumnRef {
    /// A column reachable without a database qualifier deciding anything.
    ///
    /// For a scope that is not a table reference: an already-projected result,
    /// a hidden column carrying a computed sort key, a select-list alias. 8.3
    /// treats those the way it treats a derived table — `SELECT b FROM (SELECT
    /// b FROM J2) d ORDER BY nodb.d.b` answers — and the database of a real
    /// FROM item has already been settled where that item was resolved.
    pub fn new(table: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            database: String::new(),
            table: table.into(),
            name: name.into(),
        }
    }

    /// A column of a FROM item that is in a database.
    pub fn in_database(
        database: impl Into<String>,
        table: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            database: database.into(),
            table: table.into(),
            name: name.into(),
        }
    }

    /// Whether a `db.table` names the FROM item this column came from.
    ///
    /// The database is *required* to match here, which is the mirror image of
    /// [`ColumnRef::matches`] and deliberately so: a FROM item in no database
    /// answers every database in a column reference and no database in a
    /// qualified wildcard. Measured on 8.3, over one derived table, `SELECT
    /// nodb.d.b` answers and `SELECT probe.d.*` — the database spelled right —
    /// is `Unknown table 'probe.d'`.
    pub(crate) fn in_source(&self, database: Option<&str>, table: &str) -> bool {
        self.table.eq_ignore_ascii_case(table)
            && database.is_none_or(|d| self.database.eq_ignore_ascii_case(d))
    }

    /// Whether a reference written `db.table.column` reaches this column.
    ///
    /// The one place the three parts are compared, because two comparers of
    /// one rule is how the database part came to be compared by neither.
    /// Identifiers compare case-insensitively here as they do everywhere else
    /// in this engine, which is `lower_case_table_names=1`; 8.3 on Linux
    /// defaults to 0 and compares the database and table parts exactly.
    pub(crate) fn matches(&self, written: &crate::name::ColumnParts) -> bool {
        self.name.eq_ignore_ascii_case(written.name)
            && written
                .table
                .is_none_or(|t| self.table.eq_ignore_ascii_case(t))
            // Ignored where this column's FROM item has no database, never
            // matched against the empty string: `nodb.d.b` over a derived
            // table answers on 8.3 rather than being refused for naming a
            // database the derived table is not in.
            && written
                .database
                .is_none_or(|d| self.database.is_empty() || self.database.eq_ignore_ascii_case(d))
    }
}

/// The row an expression is being evaluated against, if any.
pub struct Row<'a> {
    pub schema: &'a [ColumnRef],
    pub values: &'a [Value],
}

/// Runs a subquery on the evaluator's behalf.
///
/// What the evaluator needs from the executor but cannot reach directly.
///
/// The evaluator cannot execute a query itself without depending on the
/// executor, which depends on it. This trait is the seam that keeps the cycle
/// out of the type graph. Named locks are here for the same reason: they live
/// on the engine because they are shared between connections.
pub trait Executor {
    /// Runs a query and returns its rows.
    ///
    /// `outer` carries the row the subquery is being evaluated against, so a
    /// correlated subquery can reference the enclosing query's columns.
    fn rows(
        &self,
        query: &Query,
        session: &Session,
        outer: Option<OuterRow>,
    ) -> EngineResult<Vec<Vec<Value>>>;

    /// Applies a named-lock operation on behalf of one connection.
    fn named_lock(&self, op: LockOp, name: &str, connection_id: u32) -> Value;

    /// Writes the whole catalog to a file, or reads one back.
    ///
    /// Exposed as a function so any MySQL client can drive it: the bake step is
    /// `prisma migrate deploy` followed by one `SELECT`, with no side channel
    /// and no second protocol.
    fn state_image(&self, save: bool, path: &str) -> EngineResult<Value>;

    /// The next value for `RAND()`, in [0, 1).
    ///
    /// Deterministic, like [`Executor::next_uuid`]: a test that shuffles or
    /// samples should do the same thing every run, and nothing here needs
    /// unpredictability.
    fn next_random(&self, seed: Option<i64>) -> f64;

    /// The next value for `UUID()`.
    ///
    /// On the engine rather than the session because uniqueness has to hold
    /// across connections, and because a test that asserts on generated ids
    /// wants the same sequence every run.
    fn next_uuid(&self) -> String;
}

/// The named-lock operations MySQL exposes as functions.
///
/// Prisma's migration engine takes `GET_LOCK('prisma_migrate', ...)` before it
/// applies anything, so without these it stops before the first migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOp {
    /// GET_LOCK: 1 if acquired, 0 if another connection holds it.
    Acquire,
    /// RELEASE_LOCK: 1 if released, 0 if held by another, NULL if unheld.
    Release,
    /// IS_FREE_LOCK: 1 when nobody holds the lock.
    IsFree,
    /// IS_USED_LOCK: the holder's connection id, or NULL.
    UsedBy,
    /// RELEASE_ALL_LOCKS: how many this connection was holding.
    ReleaseAll,
}

/// An owned copy of the row a correlated subquery can see.
///
/// Owned rather than borrowed because it outlives the borrow of the row being
/// filtered, and because a subquery may itself contain one.
#[derive(Debug, Clone, Default)]
pub struct OuterRow {
    pub schema: Vec<ColumnRef>,
    pub values: Vec<Value>,
}

/// Everything an expression may read.
pub struct Context<'a> {
    pub row: Option<Row<'a>>,
    pub session: &'a Session,
    /// Absent where a subquery cannot appear, such as a column default.
    pub executor: Option<&'a dyn Executor>,
}

impl<'a> Context<'a> {
    pub fn rowless(session: &'a Session) -> Self {
        Self {
            row: None,
            session,
            executor: None,
        }
    }

    pub fn with_executor(mut self, runner: &'a dyn Executor) -> Self {
        self.executor = Some(runner);
        self
    }

    /// Runs a subquery, or explains that one cannot appear here.
    fn run(&self, query: &Query) -> EngineResult<Vec<Vec<Value>>> {
        match self.executor {
            Some(runner) => runner.rows(query, self.session, self.outer_row()),
            None => Err(unsupported("a subquery in this position")),
        }
    }

    /// The current row, in the form a correlated subquery can carry.
    fn outer_row(&self) -> Option<OuterRow> {
        let row = self.row.as_ref()?;
        Some(OuterRow {
            schema: row.schema.to_vec(),
            values: row.values.to_vec(),
        })
    }

    /// Resolves a column reference, optionally qualified by table.
    fn lookup(&self, written: &crate::name::ColumnParts) -> EngineResult<Value> {
        let Some(row) = &self.row else {
            return Err(EngineError::unknown_column(
                &written.written(),
                "field list",
            ));
        };

        let mut found: Option<&Value> = None;
        for (i, column) in row.schema.iter().enumerate() {
            if !column.matches(written) {
                continue;
            }
            // An unqualified name matching two tables is ambiguous in SQL, but
            // Prisma always qualifies, so the first match is taken rather than
            // erroring on a case that does not arise.
            found = row.values.get(i);
            break;
        }

        found
            .cloned()
            .ok_or_else(|| EngineError::unknown_column(&written.written(), "field list"))
    }
}

// Whether this thread is describing a statement rather than running it for
// its result. Set for the duration of one probe, which runs to completion on
// one thread because nothing in the engine awaits.
thread_local! {
    static PROBING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` as a probe, in which nothing with consequences may be called.
///
/// Describing a statement means running it, and a `SELECT` is not automatically
/// free of consequences: a few functions write a file, replace the catalog,
/// take a lock, or advance a counter whose whole point is to read the same on
/// every run. Refusing them here rather than by inspecting the statement text
/// catches the ones that hide — inside a view's definition, a CTE, a subquery.
/// Restored through a guard rather than a trailing assignment, for the reason
/// `Shaping` is: the flag is nested now. Typing a scalar subquery describes the
/// inner query as a probe of its own, and that happens *inside* the outer
/// statement's probe — so clearing the flag on the way out left the rest of a
/// prepare running unprobed, and `SELECT (SELECT 1), UUID()` would have taken
/// its consequence at prepare time. A guard also puts it back after a panic,
/// where the assignment did not.
pub fn probing<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            PROBING.with(|p| p.set(self.0));
        }
    }
    let _restore = Restore(PROBING.with(|p| p.replace(true)));
    f()
}

/// An EXISTS subquery with its qualified `*` items replaced, or `None` when it
/// has none to replace.
///
/// `None` rather than an unconditional copy because this runs once per row the
/// enclosing query filters, and a subquery without a qualified wildcard in it
/// — which is nearly all of them — must not pay for a clone of the tree.
fn wildcards_discarded(query: &Query) -> Option<Box<Query>> {
    if !discardable(&query.body) {
        return None;
    }
    let mut copy = query.clone();
    discard(&mut copy.body);
    Some(Box::new(copy))
}

/// Whether any branch of a query body has a qualified `*` in its select list.
///
/// Only asks whether there is anything to look at, so that the copy below can
/// be skipped; which of them are actually discarded is `discard`'s decision
/// and is made in one place. A `true` here that `discard` then leaves alone
/// costs a copy and changes no answer.
fn discardable(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(select) => select
            .projection
            .iter()
            .any(|i| matches!(i, SelectItem::QualifiedWildcard(..))),
        SetExpr::Query(inner) => discardable(&inner.body),
        SetExpr::SetOperation { left, right, .. } => discardable(left) || discardable(right),
        _ => false,
    }
}

/// Replaces every qualified `*` with the literal MySQL replaces it with.
///
/// One column, not none: the arity a branch of a UNION is compared at comes
/// from what is left, and `SELECT nope.* FROM B UNION SELECT id, aid FROM B`
/// is 1222 on 8.3 — which is only true if the first branch became one column
/// rather than zero.
fn discard(body: &mut SetExpr) {
    match body {
        SetExpr::Select(select) => {
            // The same two conditions `discardable` asks, because a HAVING
            // reads the select list and so keeps every item in it alive.
            if select.having.is_some() {
                return;
            }
            select
                .projection
                .retain(|i| !matches!(i, SelectItem::QualifiedWildcard(..)));
            if select.projection.is_empty() {
                select.projection.push(SelectItem::UnnamedExpr(Expr::Value(
                    AstValue::Number("1".to_owned(), false).into(),
                )));
            }
        }
        SetExpr::Query(inner) => discard(&mut inner.body),
        SetExpr::SetOperation { left, right, .. } => {
            discard(left);
            discard(right);
        }
        _ => {}
    }
}

/// The functions a probe must not call, because they outlive their result.
const IMPURE: &[&str] = &[
    "ultralight_snapshot",
    "ultralight_restore",
    "get_lock",
    "release_lock",
    "release_all_locks",
    "uuid",
    "rand",
];

/// Evaluates an expression to a single value.
pub fn eval(expr: &Expr, ctx: &Context<'_>) -> EngineResult<Value> {
    // A construct that extends an expression to its left can have been handed
    // more than it should hold, the same way `~` and `DIV` are, so the shape
    // is corrected before it is read. Ahead of the match rather than in each
    // of the arms, because there are eighteen of them and one left out is a
    // wrong answer with nothing to show for it.
    if let Some(fixed) = reassociated_postfix(expr) {
        return eval(&fixed, ctx);
    }
    // Bound before the match, because two arms below shadow `expr` with the
    // operand they destructure and still need the node itself.
    let expr_node = expr;
    match expr {
        Expr::Identifier(ident) if ident.value.starts_with('@') => variable(&ident.value, ctx),

        Expr::Identifier(ident) => ctx.lookup(&crate::name::ColumnParts::bare(&ident.value)),

        // `@@GLOBAL.version` parses as a compound identifier.
        Expr::CompoundIdentifier(parts)
            if parts.first().is_some_and(|p| p.value.starts_with('@')) =>
        {
            let name: Vec<&str> = parts.iter().map(|p| p.value.as_str()).collect();
            variable(&name.join("."), ctx)
        }

        Expr::CompoundIdentifier(parts) => {
            // Every part, including the database: reading only the two nearest
            // the column made `nodb.J2.b` a reference to `J2`.`b` and answered
            // rows out of a database the statement never named.
            //
            // No fallback to an unqualified lookup: an out-of-scope qualifier
            // would otherwise bind to another table's same-named column and
            // silently drive a write against the wrong rows.
            let written = crate::name::column_parts(parts);
            let shown = || {
                parts
                    .iter()
                    .map(|p| p.value.as_str())
                    .collect::<Vec<_>>()
                    .join(".")
            };
            written
                .ok_or_else(|| EngineError::unknown_column(&shown(), "field list"))
                .and_then(|written| ctx.lookup(&written))
                .map_err(|_| EngineError::unknown_column(&shown(), "field list"))
        }

        Expr::Value(v) => literal(&v.value),

        Expr::Nested(inner) => eval(inner, ctx),

        Expr::IsNull(inner) => Ok(bool_value(eval(inner, ctx)?.is_null())),
        Expr::IsNotNull(inner) => Ok(bool_value(!eval(inner, ctx)?.is_null())),
        Expr::IsTrue(inner) => Ok(bool_value(eval(inner, ctx)?.truthy() == Some(true))),
        Expr::IsFalse(inner) => Ok(bool_value(eval(inner, ctx)?.truthy() == Some(false))),
        Expr::IsNotTrue(inner) => Ok(bool_value(eval(inner, ctx)?.truthy() != Some(true))),
        Expr::IsNotFalse(inner) => Ok(bool_value(eval(inner, ctx)?.truthy() != Some(false))),

        Expr::UnaryOp { op, expr } => {
            // `~` reaches us having swallowed more than its operand, the same
            // way `DIV` does, so the shape is corrected before it is read.
            if let Some(fixed) = reassociated_unary(op, expr) {
                return eval(&fixed, ctx);
            }
            if matches!(op, UnaryOperator::BitwiseNot) {
                return bitwise_value(expr_node, ctx);
            }
            let value = eval(expr, ctx)?;
            match op {
                UnaryOperator::Not => Ok(match value.truthy() {
                    Some(b) => bool_value(!b),
                    None => Value::Null,
                }),
                // By the number MySQL reads out of the operand, like the
                // binary arithmetic below: `-'abc'` is -0, not NULL. Answering
                // NULL here became a wrong answer rather than a poor one once
                // the column was described NOT NULL — the flag promised a
                // value the evaluator would not produce.
                // Untagged first, so `-TRUE` is the integer -1 and not the
                // double -1.0 the fallback would build. MySQL types it
                // LONGLONG, and the difference is visible: `JSON_ARRAY(-TRUE)`
                // is `[-1]` there and would have been `[-1.0]`.
                // Checked at `i128` rather than negated in place, because
                // both integer tags have a value whose negation they cannot
                // hold — `-b` over a BIGINT holding `i64::MIN` and `-bu` over
                // a BIGINT UNSIGNED holding `u64::MAX` both raise 1690 on 8.3,
                // and in Rust the first of the two panicked. `false` because a
                // sign is never unsigned whatever it was given: MySQL reports
                // `-iu` LONGLONG without the flag and answers -7.
                UnaryOperator::Minus => match value.untag_bool() {
                    Value::Null => Ok(Value::Null),
                    ref exact @ (Value::Int(_) | Value::Unsigned(_)) => {
                        fitted(integer_operand(exact).map(|v| -v), false)
                    }
                    // A sign keeps the scale: `-a` over a `DECIMAL(10,2)`
                    // holding 1.50 is -1.50 on 8.3 and is declared
                    // `decimal(10,2)`, where the `f64` below printed -1.5.
                    // Subtracted from zero rather than given a `-`, because
                    // that is the same rule spelled once: a difference already
                    // takes the wider of the two scales, and zero has none.
                    Value::Decimal(ref text) => Ok(match crate::decimal::subtract("0", text) {
                        Some(negated) => Value::Decimal(negated),
                        // A `Decimal` whose text is not one, which nothing
                        // validates: read as a number, the way the fall-through
                        // below reads everything else.
                        None => Value::Float(-crate::value::numeric_prefix(text)),
                    }),
                    other => Ok(Value::Float(-other.as_number())),
                },
                UnaryOperator::Plus => Ok(value),
                other => Err(unsupported(format!("unary operator `{other}`"))),
            }
        }

        Expr::BinaryOp { left, op, right } => {
            // `DIV` reaches us mis-associated, so the shape is corrected before
            // it is read rather than evaluated as parsed.
            if let Some(fixed) = reassociated(op, left, right) {
                return eval(&fixed, ctx);
            }
            // A bitwise chain is evaluated as a chain rather than operand by
            // operand, because its intermediate values are wider than `Value`.
            if bitwise_op(op) {
                return bitwise_value(expr_node, ctx);
            }
            binary(op, eval(left, ctx)?, eval(right, ctx)?, ctx)
        }

        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => {
            let v = eval(expr, ctx)?;
            let lo = eval(low, ctx)?;
            let hi = eval(high, ctx)?;
            // `a BETWEEN lo AND hi` is two comparisons joined by `AND`, and it
            // is the *AND* that decides a NULL bound rather than the bound
            // itself: one false side settles the conjunction whatever the other
            // side is. So on 8.3 `SELECT 3 BETWEEN 4 AND NULL` is 0 — 3 >= 4 is
            // false — and `SELECT 5 BETWEEN NULL AND 4` is 0, while
            // `3 BETWEEN NULL AND 4` is NULL, because there the known side is
            // true and the answer really is unknown. Answering NULL for all
            // three, which is what taking `None` from either comparison did, is
            // a wrong answer in two of them.
            //
            // Two comparisons, but *one* comparison type: MySQL aggregates the
            // type across all three operands and then compares each side under
            // it, where the two `compare` calls below each aggregate over their
            // own pair. It is the same answer wherever the three agree, and a
            // different one where they do not — measured on 8.3,
            // `SELECT '2' BETWEEN 1 AND '10'` is 1 there and 0 here, because
            // the integer bound makes all three numeric while the pair
            // `'2' <= '10'` collates and is 0 on both servers. That is a
            // pre-existing gap of its own and not this one; it is written down
            // rather than guessed at, because aggregating three operands is a
            // rule per type family — temporals, decimals and ENUM included —
            // and half of it would be worse than none.
            //
            // Both bounds are evaluated whichever way the first comparison
            // goes, because MySQL evaluates both too: measured on 8.3,
            // `SELECT 3 BETWEEN 4 AND (@b := 1)` leaves `@b` at 1, and a high
            // bound that overflows a BIGINT still raises 1690 under a low bound
            // that has already answered false. There is no short-circuit
            // between the two comparisons to reproduce — only in the truth
            // table above them.
            //
            // The one place MySQL does skip an operand is a NULL *subject*: it
            // answers NULL without reading either bound, so
            // `SELECT NULL BETWEEN 1 AND (9223372036854775807 + 1)` is NULL on
            // 8.3 where this raises 1690. Not reproduced, deliberately — the
            // bounds are where an unsupported construct in them would announce
            // itself, and swallowing that to match an overflow is trading a
            // clear error for a silent wrong answer. It is a refusal where
            // MySQL answers, which is the safe direction, and it is written
            // down here so it is a known gap rather than a forgotten one.
            let at_least_lo = v.compare(&lo).map(|o| o != std::cmp::Ordering::Less);
            let at_most_hi = v.compare(&hi).map(|o| o != std::cmp::Ordering::Greater);
            let inside = match (at_least_lo, at_most_hi) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            };
            // `NOT BETWEEN` is the negation of that and nothing more: NULL
            // stays NULL through it, so it is not the complement of the truth
            // table but the complement of its two decided rows.
            Ok(match inside {
                Some(b) => bool_value(b != *negated),
                None => Value::Null,
            })
        }

        Expr::InList {
            expr,
            list,
            negated,
        } => {
            let needle = eval(expr, ctx)?;
            if needle.is_null() {
                return Ok(Value::Null);
            }
            let mut saw_null = false;
            for candidate in list {
                let value = eval(candidate, ctx)?;
                if value.is_null() {
                    saw_null = true;
                    continue;
                }
                if needle.compare(&value) == Some(std::cmp::Ordering::Equal) {
                    return Ok(bool_value(!*negated));
                }
            }
            // A non-match against a list containing NULL is unknown, not false.
            if saw_null {
                return Ok(Value::Null);
            }
            Ok(bool_value(*negated))
        }

        Expr::Like {
            negated,
            expr,
            pattern,
            escape_char,
            ..
        } => {
            // Resolved before the operands, because 8.3 rejects a bad ESCAPE
            // while resolving the item rather than while evaluating it:
            // `SELECT NULL LIKE 'a' ESCAPE '!!'` is 1210 there and not NULL.
            let escape = like_escape(escape_char.as_ref())?;
            let value = eval(expr, ctx)?;
            let pattern = eval(pattern, ctx)?;
            if value.is_null() || pattern.is_null() {
                return Ok(Value::Null);
            }
            let matched = functions::like(&value.as_text(), &pattern.as_text(), escape);
            Ok(bool_value(matched != *negated))
        }

        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let subject = match operand {
                Some(op) => Some(eval(op, ctx)?),
                None => None,
            };
            let mut taken = None;
            for CaseWhen { condition, result } in conditions {
                let hit = match &subject {
                    // `CASE x WHEN y` compares; `CASE WHEN c` tests truthiness.
                    Some(s) => s.compare(&eval(condition, ctx)?) == Some(std::cmp::Ordering::Equal),
                    None => eval(condition, ctx)?.truthy() == Some(true),
                };
                if hit {
                    taken = Some(result);
                    break;
                }
            }
            let Some(taken) = taken.or(else_result.as_deref()) else {
                return Ok(Value::Null);
            };
            // Untagged, because `CASE` builds a result type out of its
            // branches and a boolean is not one of the types it can build:
            // `JSON_ARRAY(CASE WHEN 1 THEN TRUE ELSE FALSE END)` is `[1]` on
            // 8.3. An ENUM survives the same construct, so the two tags part
            // company here.
            let value = eval(taken, ctx)?.untag_bool();
            Ok(unsigned_case(
                value,
                taken,
                conditions,
                else_result.as_deref(),
                ctx,
            ))
        }

        Expr::Cast {
            expr, data_type, ..
        } => {
            let value = eval(expr, ctx)?;
            functions::cast(value, data_type, &expr.to_string())
        }

        // A scalar subquery: one row, one column.
        Expr::Subquery(query) => {
            let rows = ctx.run(query)?;
            if rows.len() > 1 {
                return Err(EngineError::new(
                    1242,
                    "21000",
                    "Subquery returns more than 1 row",
                ));
            }
            // Untagged on the way out, because a scalar subquery is one of
            // the places MySQL stops reading an ENUM as its ordinal:
            // `(SELECT e FROM t) + 0` is 0 there where the bare `e + 0` is 1.
            Ok(rows
                .into_iter()
                .next()
                .and_then(|row| row.into_iter().next())
                .map(Value::untag_enum)
                .unwrap_or(Value::Null))
        }

        Expr::Exists { subquery, negated } => {
            // MySQL throws an EXISTS subquery's select list away before it
            // expands any `*` in it, so `EXISTS (SELECT o.* FROM t)` is legal
            // on 8.3 even where nothing `t` provides is called `o`. Measured:
            // it holds through DISTINCT, GROUP BY, ORDER BY and a UNION, a
            // *named* column in the same position is still resolved and still
            // 1054, and a HAVING keeps the list alive long enough for the `*`
            // to be resolved after all.
            //
            // Done here rather than by softening the projection's own check,
            // because this is the only place that knows the columns will
            // never be looked at.
            let discarded = wildcards_discarded(subquery);
            let rows = ctx.run(discarded.as_deref().unwrap_or(subquery))?;
            Ok(bool_value(!rows.is_empty() != *negated))
        }

        Expr::InSubquery {
            expr,
            subquery,
            negated,
        } => {
            let needle = eval(expr, ctx)?;
            if needle.is_null() {
                return Ok(Value::Null);
            }
            let rows = ctx.run(subquery)?;
            let mut saw_null = false;
            for row in rows {
                let Some(candidate) = row.into_iter().next() else {
                    continue;
                };
                if candidate.is_null() {
                    saw_null = true;
                    continue;
                }
                if needle.compare(&candidate) == Some(std::cmp::Ordering::Equal) {
                    return Ok(bool_value(!*negated));
                }
            }
            // As with an IN list, a miss against a set containing NULL is
            // unknown rather than false.
            if saw_null {
                return Ok(Value::Null);
            }
            Ok(bool_value(*negated))
        }

        // sqlparser models these as their own nodes rather than as function
        // calls, because SQL gives them keyword argument syntax. They are the
        // same functions, so they are routed to the same table rather than
        // reimplemented.
        //
        // `CEIL` and `FLOOR` are here for the same reason and had no arm at
        // all, so `SELECT CEIL(1.5)` was refused outright while the `"ceil"`
        // entry in the function table sat unreachable — the engine advertised
        // a function no statement could call. `CEILING` is spelled the same
        // way to MySQL and parses as an ordinary call, which is why only it
        // worked.
        Expr::Ceil { expr, field } => rounded_to_integer("ceil", expr, field, ctx),
        Expr::Floor { expr, field } => rounded_to_integer("floor", expr, field, ctx),

        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            ..
        } => {
            let mut args = vec![eval(expr, ctx)?];
            args.push(match substring_from {
                Some(e) => eval(e, ctx)?,
                None => Value::Int(1),
            });
            if let Some(e) = substring_for {
                args.push(eval(e, ctx)?);
            }
            functions::call("substring", args, ctx.session)
        }

        Expr::Trim {
            expr,
            trim_where,
            trim_what,
            trim_characters,
        } => {
            if trim_characters.is_some() {
                return Err(unsupported("TRIM with a character list"));
            }
            let subject = eval(expr, ctx)?;
            if subject.is_null() {
                return Ok(Value::Null);
            }
            let what = match trim_what {
                Some(w) => {
                    let value = eval(w, ctx)?;
                    if value.is_null() {
                        return Ok(Value::Null);
                    }
                    value.as_text()
                }
                None => " ".to_owned(),
            };
            // MySQL strips whole repetitions of the string, not any character
            // in it, and an empty one would loop forever.
            let mut text = subject.as_text();
            if !what.is_empty() {
                use sqlparser::ast::TrimWhereField as Side;
                let lead = matches!(trim_where, None | Some(Side::Both) | Some(Side::Leading));
                let trail = matches!(trim_where, None | Some(Side::Both) | Some(Side::Trailing));
                if lead {
                    while let Some(rest) = text.strip_prefix(&what) {
                        text = rest.to_owned();
                    }
                }
                if trail {
                    while let Some(rest) = text.strip_suffix(&what) {
                        text = rest.to_owned();
                    }
                }
            }
            Ok(Value::text(text))
        }

        Expr::Position { expr, r#in } => {
            let args = vec![eval(expr, ctx)?, eval(r#in, ctx)?];
            functions::call("position", args, ctx.session)
        }

        Expr::Extract { field, expr, .. } => extract(field, expr, ctx),

        // `x REGEXP y` and `x RLIKE y`, which are the same operator.
        Expr::RLike {
            negated,
            expr,
            pattern,
            ..
        } => {
            // `x REGEXP BINARY y` asks for a case-sensitive match, which is
            // not what this evaluates, so it is refused rather than answered
            // with the wrong comparison. The parser reads the operator as a
            // cast, which is where it has to be recognised.
            if matches!(
                pattern.as_ref(),
                Expr::Cast { data_type, .. }
                    if matches!(
                        data_type,
                        sqlparser::ast::DataType::Binary(_)
                            | sqlparser::ast::DataType::Varbinary(_)
                            | sqlparser::ast::DataType::Blob(_)
                    )
            ) {
                return Err(unsupported("REGEXP against a BINARY pattern"));
            }
            let subject = eval(expr, ctx)?;
            let pattern = eval(pattern, ctx)?;
            if subject.is_null() || pattern.is_null() {
                return Ok(Value::Null);
            }
            let matched = functions::call("regexp_like", vec![subject, pattern], ctx.session)?;
            Ok(bool_value((matched.truthy() == Some(true)) != *negated))
        }

        // `x COLLATE c` compares x under a different collation. Which
        // collations exist, which this engine can honour, and why the rest are
        // refused rather than ignored, is `collation` — the column-level and
        // table-level clauses read the same one, so the three cannot drift
        // apart again.
        Expr::Collate { expr, collation } => {
            crate::collation::read(collation)?;
            eval(expr, ctx)
        }

        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_lowercase();

            if PROBING.with(std::cell::Cell::get) && IMPURE.contains(&name.as_str()) {
                return Err(unsupported(format!(
                    "{name}() in a statement being described rather than run"
                )));
            }

            // These read part of their argument list as syntax rather than as
            // values: an `INTERVAL`, or a bare unit keyword that would
            // otherwise resolve as a column. They are handled before the
            // arguments are evaluated for that reason.
            if let "date_add" | "date_sub" | "adddate" | "subdate" | "timestampdiff"
            | "timestampadd" = name.as_str()
            {
                return temporal_call(&name, f, ctx);
            }

            let args = function_args(f, ctx)?;

            // Named locks need engine state rather than session state, so they
            // go through the executor seam instead of the function table.
            let op = match name.as_str() {
                "get_lock" => Some(LockOp::Acquire),
                "release_lock" => Some(LockOp::Release),
                "is_free_lock" => Some(LockOp::IsFree),
                "is_used_lock" => Some(LockOp::UsedBy),
                "release_all_locks" => Some(LockOp::ReleaseAll),
                _ => None,
            };
            if let "ultralight_snapshot" | "ultralight_restore" = name.as_str() {
                let Some(engine) = ctx.executor else {
                    return Err(unsupported(format!("{name}() here")));
                };
                let Some(path) = args.first().filter(|v| !v.is_null()) else {
                    return Err(unsupported(format!("{name}() without a path")));
                };
                // An image is the whole catalog, and a transaction is a
                // promise about part of it. Snapshotting inside one would bake
                // rows that a rollback then removes from the live catalog but
                // not from the file; restoring inside one leaves the rollback
                // replaying pre-transaction tables over a catalog that has
                // been replaced wholesale, which lands somewhere that is
                // neither the image nor where it started.
                if ctx.session.in_transaction {
                    return Err(EngineError::new(
                        1105,
                        "HY000",
                        format!("{name}() cannot be called inside a transaction"),
                    ));
                }
                return engine.state_image(name == "ultralight_snapshot", &path.as_text());
            }

            if name == "rand" {
                let Some(engine) = ctx.executor else {
                    return Err(unsupported("rand() here"));
                };
                let seed = args.first().and_then(|v| v.as_int());
                return Ok(Value::Float(engine.next_random(seed)));
            }

            if name == "uuid" {
                let Some(engine) = ctx.executor else {
                    return Err(unsupported("uuid() here"));
                };
                return Ok(Value::text(engine.next_uuid()));
            }

            if let Some(op) = op {
                let Some(engine) = ctx.executor else {
                    return Err(unsupported(format!("{name}() here")));
                };
                let lock_name = args.first().map(|v| v.as_text()).unwrap_or_default();
                return Ok(engine.named_lock(op, &lock_name, ctx.session.connection_id));
            }

            functions::call(&name, args, ctx.session)
        }

        // `@@var` and `@var`. sqlparser models both as identifiers with the
        // sigil retained, so they are handled here rather than as functions.
        Expr::Prefixed { .. } => Err(unsupported("prefixed expressions")),

        other => Err(unsupported(format!("expression `{other}`"))),
    }
}

/// A function's arguments as expressions, for the few that read their
/// arguments as syntax rather than as values.
fn argument_exprs(f: &sqlparser::ast::Function) -> Vec<Expr> {
    let FunctionArguments::List(list) = &f.args else {
        return Vec::new();
    };
    list.args
        .iter()
        .filter_map(|arg| match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
            | FunctionArg::Named {
                arg: FunctionArgExpr::Expr(e),
                ..
            }
            | FunctionArg::ExprNamed {
                arg: FunctionArgExpr::Expr(e),
                ..
            } => Some(e.clone()),
            _ => None,
        })
        .collect()
}

/// Evaluates a function's arguments left to right.
fn function_args(f: &sqlparser::ast::Function, ctx: &Context<'_>) -> EngineResult<Vec<Value>> {
    let FunctionArguments::List(list) = &f.args else {
        // `COUNT(*)` and bare `NOW` arrive with no argument list.
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(list.args.len());
    for arg in &list.args {
        match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
            | FunctionArg::Named {
                arg: FunctionArgExpr::Expr(e),
                ..
            }
            | FunctionArg::ExprNamed {
                arg: FunctionArgExpr::Expr(e),
                ..
            } => out.push(eval(e, ctx)?),
            // `COUNT(*)`: the wildcard carries no value of its own.
            _ => out.push(Value::Int(1)),
        }
    }
    Ok(out)
}

/// The value a numeric literal *is*, which for most of them is exact.
///
/// MySQL reads a number written without an exponent exactly, and types it by
/// how far it reaches: `18446744073709551615` is a BIGINT UNSIGNED there and
/// `18446744073709551616` a DECIMAL, and both answer every digit — where
/// reading either as a double rounds the operand before anything is done with
/// it, so the second printed 1.8446744073709552e19.
///
/// A point makes it a DECIMAL however short it is, and the *written* scale is
/// the one it carries: `1.50` is `decimal(3,2)` on 8.3 and prints 1.50, so the
/// text is kept rather than a number parsed out of it. That is what lets
/// `a + 0.5` over a `DECIMAL(10,2)` be 2.00 — the rule reads both operands'
/// scales, and a double has none.
///
/// An exponent is the exception and stays approximate, because MySQL types it
/// DOUBLE: `1.5e3` is a double there and `1.5` is not.
fn number_literal(text: &str) -> Value {
    if let Ok(v) = text.parse::<i64>() {
        return Value::Int(v);
    }
    if let Ok(v) = text.parse::<u64>() {
        return Value::Unsigned(v);
    }
    // Spelled out rather than kept verbatim: MySQL prints `.5` as 0.5 and
    // `007.5` as 7.5, and keeps every place that was written — 1.5 to 37
    // places stays at 37 there, which is why the scale is the text's own and
    // is not clamped here.
    if let Some(exact) = crate::decimal::rescale_wide(text, crate::decimal::scale_of(text)) {
        return Value::Decimal(exact);
    }
    Value::Float(text.parse::<f64>().unwrap_or(0.0))
}

/// The character a `LIKE ... ESCAPE` clause names, or the default.
///
/// Read from the literal's *value* and not from its rendering. `Display` on a
/// `Value` prints the SQL that would produce it, quotes included, so
/// `to_string().chars().next()` handed back the opening quote and every
/// ESCAPE clause in this engine escaped with `'` whatever was written —
/// `'a%' LIKE 'a''%' ESCAPE '!'` answered 1 where 8.3 answers 0 (#0093).
///
/// Measured against 8.3, all of which the rendering could not have got right:
///
/// - `ESCAPE '!'` is `!`. The length is counted in characters and not bytes,
///   so `ESCAPE 'é'` is legal on a utf8mb4 connection and 1210 on a latin1 one.
/// - `ESCAPE ''` turns escaping *off* rather than selecting a character:
///   `'a%' LIKE 'a\%' ESCAPE ''` is 0, where the same pattern with no ESCAPE
///   clause is 1.
/// - `ESCAPE NULL` is not that. It leaves the default backslash in place, so
///   `'a%' LIKE 'a\%' ESCAPE NULL` is 1 — the one place here where NULL does
///   not propagate.
/// - A non-string is converted first: `ESCAPE 1` is `1`, `ESCAPE TRUE` is `1`,
///   `ESCAPE x'21'` is `!`, and `ESCAPE 1.5` is three characters and an error.
/// - Anything longer than one character is ER_WRONG_ARGUMENTS.
/// - A *binary* string is counted in bytes, because that is what one of its
///   characters is: `ESCAPE x'FF'` is one character and legal on 8.3, and
///   `ESCAPE x'C3A9'` is two and 1210 — where the same two bytes written as
///   text, `ESCAPE 'é'`, are one character and legal. `ESCAPE x''` is off,
///   like `ESCAPE ''`.
///
/// Called at resolve time as well as per row, by `exec::malformed_escape`,
/// because 8.3 raises 1210 for a statement it never evaluates: over an empty
/// table, and down a CASE branch that is not taken.
pub(crate) fn like_escape(clause: Option<&ValueWithSpan>) -> EngineResult<Option<char>> {
    // MySQL escapes with a backslash unless told otherwise. ORMs escape user
    // input that way and omit the ESCAPE clause, so leaving this as None
    // makes `contains` filters return nothing.
    //
    // NO_BACKSLASH_ESCAPES takes it away on 8.3, and this engine does not
    // model that mode at all — not here and not in the tokenizer — so the
    // default does not consult it. Measured with the pattern held constant:
    // `SELECT 'a%' LIKE 'a\%', HEX('a\%')` answers `1, 615C25` by default and
    // `0, 615C25` under the mode, so the same three bytes stop escaping.
    const DEFAULT: Option<char> = Some('\\');

    let Some(clause) = clause else {
        return Ok(DEFAULT);
    };
    let named = literal(&clause.value)?;
    if named.is_null() {
        return Ok(DEFAULT);
    }

    // A binary string is measured and read as bytes, before `as_text` gets to
    // it. Going through `as_text` decodes lossily, which turned `ESCAPE x'FF'`
    // and `ESCAPE x'FE'` into the same one-character escape — U+FFFD, a
    // character neither of them names and no pattern can contain — and let
    // `ESCAPE x'C3A9'` through as one character where 8.3 counts two and
    // raises 1210.
    if let Value::Bytes(bytes) = &named {
        return match bytes[..] {
            [] => Ok(None),
            [byte] if byte.is_ascii() => Ok(Some(char::from(byte))),
            // 8.3 escapes with the byte itself. This engine matches over
            // `char`s — both operands reach `functions::like` through
            // `as_text` — so it has no way to name a byte that is not one,
            // and refuses rather than escaping with something else. The
            // collation work that would fix it is #0134.
            [_] => Err(unsupported(
                "a LIKE escape that is a byte rather than a character",
            )),
            _ => Err(wrong_escape()),
        };
    }

    let text = named.as_text();
    let mut chars = text.chars();
    let first = chars.next();
    if chars.next().is_some() {
        return Err(wrong_escape());
    }
    // `None` from an empty string, which is the same "no escape character" the
    // matcher already understands rather than a separate state.
    Ok(first)
}

/// ER_WRONG_ARGUMENTS, which 8.3 words for the clause rather than for the
/// value it was given.
fn wrong_escape() -> EngineError {
    EngineError::new(1210, "HY000", "Incorrect arguments to ESCAPE")
}

/// Converts a literal from the AST.
pub fn literal(value: &AstValue) -> EngineResult<Value> {
    Ok(match value {
        AstValue::Number(n, _) => number_literal(n),
        AstValue::SingleQuotedString(s)
        | AstValue::DoubleQuotedString(s)
        | AstValue::TripleSingleQuotedString(s)
        | AstValue::TripleDoubleQuotedString(s)
        | AstValue::EscapedStringLiteral(s)
        | AstValue::NationalStringLiteral(s) => Value::Text(s.clone()),
        AstValue::HexStringLiteral(s) => {
            let bytes = (0..s.len())
                .step_by(2)
                .filter_map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
                .collect();
            Value::Bytes(bytes)
        }
        // Tagged, not `Int(0|1)`: MySQL converts the keyword to the JSON
        // boolean and the literal 1 to the JSON number, so `CAST('true' AS
        // JSON) = TRUE` is 1 there and `= 1` is 0. Untagged the two operands
        // were the same value and the first answered 0.
        AstValue::Boolean(b) => Value::Bool(*b),
        AstValue::Null => Value::Null,
        AstValue::Placeholder(p) => {
            return Err(EngineError::parse(format!(
                "unbound parameter placeholder `{p}`"
            )))
        }
        other => return Err(unsupported(format!("literal `{other}`"))),
    })
}

/// Resolves `@user` and `@@system` variables.
///
/// An unset user variable reads as NULL, as in MySQL. An unknown system
/// variable is an error there too, and guessing a value would hide a real
/// incompatibility from whatever asked for it.
fn variable(name: &str, ctx: &Context<'_>) -> EngineResult<Value> {
    if let Some(rest) = name.strip_prefix("@@") {
        return ctx.session.system_variable(rest).ok_or_else(|| {
            EngineError::new(1193, "HY000", format!("Unknown system variable '{rest}'"))
        });
    }
    match name.strip_prefix('@') {
        Some(rest) => Ok(ctx.session.user_variable(rest)),
        None => Err(EngineError::unknown_column(name, "field list")),
    }
}

/// A constant literal, if the expression is one.
///
/// Used to tell a column default that can be stored once from one that must be
/// evaluated per row, such as `CURRENT_TIMESTAMP`.
pub fn literal_of(e: &Expr) -> Option<Value> {
    match e {
        Expr::Value(v) => literal(&v.value).ok(),
        Expr::Nested(inner) => literal_of(inner),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match literal_of(expr)? {
            Value::Int(v) => Some(Value::Int(-v)),
            Value::Float(v) => Some(Value::Float(-v)),
            // The same subtraction the evaluator's unary minus does, so a
            // `DEFAULT -1.50` is stored at the scale it was written with.
            Value::Decimal(text) => crate::decimal::subtract("0", &text).map(Value::Decimal),
            _ => None,
        },
        _ => None,
    }
}

/// The answer to a yes-or-no question, tagged as one.
///
/// It reads as 1 or 0 everywhere a client can see — `SELECT 1 = 1` sends the
/// character `1` and is a LONGLONG column — and as a JSON boolean inside a
/// document, which is the one place MySQL keeps the two apart. Measured on
/// 8.3: `JSON_ARRAY(1 = 1)`, `JSON_ARRAY(NOT 0)`, `JSON_ARRAY(1 AND 1)`,
/// `JSON_ARRAY(1 IS TRUE)` and `JSON_ARRAY(1 IN (1, 2))` are all `[true]`.
///
/// Every comparison, logical operator, postfix test and `EXISTS` already came
/// through here, which is why the tag is applied at this one funnel rather
/// than at the `TRUE` keyword alone — the keyword is the smaller half of the
/// surface.
pub fn bool_value(b: bool) -> Value {
    Value::Bool(b)
}

fn binary(
    op: &BinaryOperator,
    left: Value,
    right: Value,
    ctx: &Context<'_>,
) -> EngineResult<Value> {
    use std::cmp::Ordering;
    use BinaryOperator as B;

    // Comparison against NULL is unknown, not false. Getting this wrong makes
    // `WHERE x <> 'a'` silently drop rows where x is NULL.
    let cmp = |ord: &[Ordering]| -> Value {
        match left.compare(&right) {
            Some(o) => bool_value(ord.contains(&o)),
            None => Value::Null,
        }
    };

    Ok(match op {
        B::Eq => cmp(&[Ordering::Equal]),
        B::NotEq => cmp(&[Ordering::Less, Ordering::Greater]),
        B::Lt => cmp(&[Ordering::Less]),
        B::LtEq => cmp(&[Ordering::Less, Ordering::Equal]),
        B::Gt => cmp(&[Ordering::Greater]),
        B::GtEq => cmp(&[Ordering::Greater, Ordering::Equal]),

        // `<=>` is MySQL's NULL-safe equality: two NULLs are equal.
        B::Spaceship => match (left.is_null(), right.is_null()) {
            (true, true) => bool_value(true),
            (false, false) => bool_value(left.compare(&right) == Some(Ordering::Equal)),
            _ => bool_value(false),
        },

        // AND is false if either side is false, even when the other is NULL.
        B::And => match (left.truthy(), right.truthy()) {
            (Some(false), _) | (_, Some(false)) => bool_value(false),
            (Some(true), Some(true)) => bool_value(true),
            _ => Value::Null,
        },
        // OR is true if either side is true, even when the other is NULL.
        B::Or => match (left.truthy(), right.truthy()) {
            (Some(true), _) | (_, Some(true)) => bool_value(true),
            (Some(false), Some(false)) => bool_value(false),
            _ => Value::Null,
        },
        // XOR is the connective neither side can settle on its own: flipping
        // an unknown operand flips the answer, so one NULL is enough to make
        // the whole thing NULL where `1 OR NULL` is 1 and `0 AND NULL` is 0.
        //
        // Its operands are truth values and not bits — measured on 8.3,
        // `2 XOR 0` is 1 and `2.5 XOR 0` is 1 and `'abc' XOR 1` is 1, none of
        // which is what the bitwise `^` would say. `bool_value` for the same
        // reason every other connective uses it: `JSON_ARRAY(1 XOR 0)` is
        // `[true]` there, not `[1]`.
        B::Xor => match (left.truthy(), right.truthy()) {
            (Some(a), Some(b)) => bool_value(a != b),
            _ => Value::Null,
        },

        B::Plus | B::Minus | B::Multiply | B::Divide | B::Modulo | B::MyIntegerDivide => {
            arithmetic(op, &left, &right, ctx.session.div_precision_increment())?
        }

        // The tokenizer always produces StringConcat for `||`, but what it
        // means depends on sql_mode: concatenation under PIPES_AS_CONCAT, and
        // logical OR otherwise. Picking one unconditionally makes half of all
        // deployments silently compute the wrong answer.
        B::StringConcat => {
            if ctx.session.sql_mode().pipes_as_concat {
                if left.is_null() || right.is_null() {
                    Value::Null
                } else {
                    Value::Text(format!("{}{}", left.as_text(), right.as_text()))
                }
            } else {
                match (left.truthy(), right.truthy()) {
                    (Some(true), _) | (_, Some(true)) => bool_value(true),
                    (Some(false), Some(false)) => bool_value(false),
                    _ => Value::Null,
                }
            }
        }

        // `->` extracts, `->>` extracts and unquotes. Both are shorthand for
        // the function pair, so they go through the same code.
        B::Arrow | B::LongArrow => {
            let extracted = functions::call("json_extract", vec![left, right], ctx.session)?;
            if matches!(op, B::LongArrow) {
                functions::call("json_unquote", vec![extracted], ctx.session)?
            } else {
                extracted
            }
        }

        other => return Err(unsupported(format!("operator `{other}`"))),
    })
}

/// How tightly an operator binds, in MySQL's order rather than sqlparser's.
///
/// Higher binds tighter. Zero is not a level: it means the operator takes no
/// part in any disagreement between the two, so nothing below moves it and
/// nothing is moved past it.
///
/// Every order here was measured against MySQL 8.3, because sqlparser's
/// numbers are wrong in both directions and the manual does not cover `||` at
/// all. `2*3||4` is 68 rather than 64, so `||` where it concatenates binds
/// tighter than `*`; `'x' || doc->'$.a'` is `x5`, so a path extraction binds
/// tighter still; and `2^3||4` is 32, `2^3+1` is 2, `1+2<<3` is 24, `1<<2&3`
/// is 0 and `1&2|3` is 3, which is the bitwise order `^`, then `* / DIV %`,
/// then `+ -`, then `<< >>`, then `&`, then `|`.
///
/// The four loosest were added for #0056 and are measured the same way.
/// `1 = 2 XOR 3` is 1, so a comparison binds tighter than `XOR`;
/// `2 XOR 3 AND 0` is 1, so `AND` binds tighter still; and `1 OR 1 XOR 1` is
/// 1, so `OR` is looser. `XOR` had to reach the table at all because
/// sqlparser puts it between `+` and `&`, which is on the wrong side of every
/// comparison, every bitwise operator and `AND` alike. `AND`, `OR` and the
/// comparisons come with it rather than on their own account: a chain stops
/// at an operator with no level, so `XOR` cannot be moved past what is not in
/// it.
fn precedence(op: &BinaryOperator) -> u8 {
    use BinaryOperator as B;
    match op {
        B::Arrow | B::LongArrow => 14,
        // Two operators wearing one token, and sqlparser reads the token at
        // one precedence — the one it gives `*`. Concatenating, that is nearly
        // right and this row moves it above `*`; meaning OR, it is as wrong as
        // a precedence can be, and `1 || 1 AND 0` is 1 in MySQL where the
        // parsed shape `(1 || 1) AND 0` is 0.
        //
        // Read from the statement rather than from a session, which is where
        // `binary` reads it to decide what `||` *does*. Two readings of one
        // flag, so `eval` has to run inside a `crate::exec::Source` scope or
        // they can disagree and `7 DIV 2 || 3` would be shaped as OR and then
        // concatenated. Both entry points that parse set that scope.
        B::StringConcat if crate::exec::pipes_as_concat() => 13,
        B::BitwiseXor => 12,
        B::Multiply | B::Divide | B::Modulo | B::MyIntegerDivide => 11,
        B::Plus | B::Minus => 10,
        B::PGBitwiseShiftLeft | B::PGBitwiseShiftRight => 9,
        B::BitwiseAnd => 8,
        B::BitwiseOr => 7,
        B::Eq | B::NotEq | B::Lt | B::LtEq | B::Gt | B::GtEq | B::Spaceship => COMPARISON,
        B::And => 3,
        B::Xor => 2,
        // `OR`, and `||` wherever it means OR. MySQL has one level looser
        // still — `:=` — which this engine does not answer, so this is the
        // bottom of the chain rather than a level with something below it.
        B::Or | B::StringConcat => 1,
        _ => 0,
    }
}

/// Where MySQL puts the prefix `NOT`: looser than a comparison and tighter
/// than `AND`. Not a `BinaryOp`, and not `PREFIX` either — `~` binds to the
/// operand beside it and `NOT` binds to a whole comparison.
///
/// sqlparser reads its operand at a precedence that stops at `AND` and `OR`
/// and swallows everything else, so the one thing it lets in that MySQL keeps
/// out is `XOR`. Measured on 8.3: `NOT 0 XOR 1 AND 0` is 1, which is
/// `(NOT 0) XOR (1 AND 0)`; the parsed `NOT (0 XOR 1) AND 0` is 0. So is
/// `NOT 0 XOR 0 AND 0`, and `NOT NULL XOR 1 AND 0` is NULL where the parsed
/// shape says 0. Everything tighter stays inside it: `NOT 0 = 0` is 0,
/// `NOT 1 + 2` is 0, `NOT 0 | 1` is 0 and `NOT 2 IS NULL` is 1.
const NOT: u8 = 4;

/// Where MySQL puts every comparison, and the `IS` tests along with them.
///
/// `IS NULL` and its family are not `BinaryOp` nodes but they bind here, and
/// left-associatively: `0 = 0 IS NULL` is 0 on 8.3, which is `(0 = 0) IS NULL`
/// and not `0 = (0 IS NULL)`.
const COMPARISON: u8 = 5;

/// One level tighter, where MySQL's grammar puts `IN`, `BETWEEN`, `LIKE` and
/// `REGEXP` — a `predicate`, which a `bool_pri` compares *to*.
///
/// A level of its own because the difference is visible: `2 = 2 IN (1)` is 0
/// on 8.3, which is `2 = (2 IN (1))`. Reading them at `COMPARISON` and letting
/// left-associativity decide would give `(2 = 2) IN (1)`, which is 1.
/// Measured the same way for each: `2 = 2 LIKE 1`, `2 = 2 REGEXP '1'` and
/// `3 = 3 BETWEEN 0 AND 1` are all 0.
const PREDICATE: u8 = 6;

/// Tighter than any binary operator, which is where MySQL puts a prefix.
///
/// `~1*2` is `(~1)*2` there — it overflows and is refused — where `~(1*2)` is
/// 18446744073709551613 and is not.
const PREFIX: u8 = 15;

/// Re-associates a tree sqlparser grouped differently from MySQL.
///
/// Three separate disagreements, and they are wrong in different ways. `DIV`
/// is parsed by a dialect hook that reads its right operand with `parse_expr`,
/// which takes everything to the end of the expression: `7 DIV 2 + 1` arrives
/// as `7 DIV (2 + 1)` and answers 2 where MySQL answers 4. `->` and `->>` are
/// given nearly the loosest precedence there is, where MySQL treats a path
/// extraction as a primary: `doc->'$.b' = 6` arrives as `doc -> ('$.b' = 6)`
/// and asks for the path `0`. And sqlparser's bitwise order is Postgres's, not
/// MySQL's — `^` is put *below* `+` and `&` *above* `<<`, so `2 ^ 3 + 1` is
/// grouped `2 ^ (3 + 1)` and `1 & 2 << 3` is grouped `(1 & 2) << 3`. All of
/// them are silent: a `WHERE` over any of them matches the wrong rows.
///
/// A wrapper `Dialect` that fixed this at the source is not an option here —
/// see `crate::dialect` for why — so the shape is corrected after parsing,
/// wherever the tree is read.
///
/// Two steps, because the two problems are not the same problem. What `DIV`
/// and `->` swallowed can be something that is not an operator at all — a
/// postfix test, an `IN` list, a `BETWEEN` — and only a walk can put those
/// back. What is left after that is a chain of operators, and a chain is
/// regrouped wholesale rather than one pair at a time.
///
/// `None` when the tree already means what MySQL means, which is also what
/// stops the caller recursing forever: the answer is compared against the node
/// that came in.
pub(crate) fn reassociated(op: &BinaryOperator, left: &Expr, right: &Expr) -> Option<Expr> {
    let level = precedence(op);
    if level == 0 {
        return None;
    }
    // Nearly every node that gets here is already the shape MySQL means, and
    // recognising that is a walk over the chain rather than the two steps
    // below, which deep-clone both operands and rebuild the tree before they
    // can conclude the same thing. This runs on every arithmetic node of every
    // row, so `a + b` doing that work is the difference between a chain of
    // twelve terms over twenty thousand rows costing 10 ms and 310 ms.
    if grouped_node(level, left, right).is_some() {
        return None;
    }
    let node = Expr::BinaryOp {
        left: Box::new(left.clone()),
        op: op.clone(),
        right: Box::new(right.clone()),
    };

    // Step one: give back what the right operand swallowed and is not part of
    // any chain. `7 DIV 2 IS NULL` arrives as `7 DIV (2 IS NULL)`, and the
    // `IS NULL` is a node the regrouping below cannot see into.
    let mut fixed = right.clone();
    let wrap = |operand: Expr| Expr::BinaryOp {
        left: Box::new(left.clone()),
        op: op.clone(),
        right: Box::new(operand),
    };
    let walked = if take_leftmost(&mut fixed, &wrap, level) {
        fixed
    } else {
        node.clone()
    };

    // Step two: regroup the operator chain by MySQL's precedence.
    let grouped = regrouped(&walked);
    (grouped != node).then_some(grouped)
}

/// The chain rooted at `e`, grouped the way MySQL groups it.
///
/// Rotating one disagreement at a time cannot do this. A rotation has to know
/// the *root* of the left operand to decide, and the left operand does not
/// have its final root until it has been corrected itself: `2 ^ 3 + 2 ^ 3`
/// arrives as `(2 ^ (3 + 2)) ^ 3`, whose left corrects to `(2 ^ 3) + 2` — but
/// the decision was already made against the `^` that was there before, so the
/// outer `^` swallowed the lot and answered 0 where MySQL answers 2. A fuzz of
/// 6000 random three- to five-term expressions found roughly 150 such shapes.
///
/// So the chain is rebuilt instead. The tree sqlparser produced is a valid
/// reading of the same tokens under *its* precedences, and an in-order walk
/// hands those tokens back; parsing them again under MySQL's is one rule for
/// every pair at once, with no order to get wrong.
fn regrouped(e: &Expr) -> Expr {
    let mut operands = Vec::new();
    let mut ops = Vec::new();
    flatten(e, &mut operands, &mut ops);
    if ops.is_empty() {
        return e.clone();
    }
    let mut operands = operands.into_iter();
    let mut ops = ops.into_iter().peekable();
    build(&mut operands, &mut ops, 0)
}

/// The loosest operator in `e`, when `e` is already grouped MySQL's way.
///
/// `None` when it is not, and also when it holds something this cannot answer
/// for — the slow path decides those, so being unsure only costs the work that
/// was being done unconditionally before.
///
/// The tree that precedence climbing builds for a chain is the only one where
/// every node has its left operand no looser than itself and its right operand
/// strictly tighter, so checking that at each node is checking the whole
/// answer: the root of a correct chain is the *last* of its loosest operators,
/// and those two conditions say exactly that. `build` is what has to agree
/// with this, and it does by construction — it takes operators while their
/// level is at or above the minimum and reads each right operand at one level
/// above the operator, so what it returns satisfies both.
fn grouped(e: &Expr) -> Option<u8> {
    use Expr as E;
    // A construct that extends an expression to its left binds at `COMPARISON`
    // or at `PREDICATE`, and is grouped when both its operands are and neither
    // holds anything looser. Left-associative like every operator here, so the
    // operand on the left may tie with it and the one after the keyword may
    // not.
    //
    // Reporting a level rather than refusing outright — which this whole list
    // used to do — is what keeps `a IS NULL AND b = 1` on the fast path now
    // that `AND` has a level of its own. The refusal is still there wherever
    // it was doing work: `7 DIV (2 IS NULL)` is an operand at `COMPARISON`
    // under an operator far tighter, which `grouped_node` turns down on the
    // level alone, and only `take_leftmost` puts the `IS NULL` back outside.
    if let Some((level, leading, trailing)) = postfix_operands(e) {
        return (grouped(leading)? >= level
            && trailing.is_none_or(|t| grouped(t).is_some_and(|l| l > level)))
        .then_some(level);
    }
    match e {
        // Zero is not a level, and reporting it is what refuses an operator
        // that has none: no link in a chain is that loose, so a caller asking
        // for something no looser than itself on the left, or tighter on the
        // right, is answered no either way. That is the refusal `doc->'$.b' =
        // 6` needs — it arrives as `doc -> ('$.b' = 6)`, and the `=` is not a
        // link in the chain but something the path extraction swallowed.
        E::BinaryOp { left, op, right } => grouped_node(precedence(op), left, right),
        // `~` reads its operand at the precedence of `+` where MySQL binds it
        // to the operand next to it, so one holding an operator is holding
        // part of the chain: `3 ^ ~1 % 5` hides the `%` inside it. Anything an
        // expression can extend to its left is what `reassociated_unary` moves
        // out, and asking it directly would clone the operand to answer — the
        // clone this exists to avoid — so the same question is asked of the
        // shape instead.
        E::UnaryOp {
            op: UnaryOperator::BitwiseNot,
            expr,
        } => (!matches!(**expr, E::BinaryOp { .. }) && grouped(expr).is_some()).then_some(u8::MAX),
        // `NOT` is a level of its own, so a node holding something looser than
        // it is holding part of the chain: `NOT 0 XOR 1 AND 0` arrives as
        // `NOT (0 XOR 1) AND 0` and the `XOR` belongs outside the `NOT`.
        // Unlike `~`, an operand that *is* an operator is the normal case —
        // `NOT 0 = 0` is `NOT (0 = 0)` in both readings — so what is asked is
        // whether that operator binds tighter, not whether there is one.
        E::UnaryOp {
            op: UnaryOperator::Not,
            expr,
        } => (grouped(expr)? > NOT).then_some(NOT),
        // Everything else is one operand and ends the chain: a literal, a
        // column, a call, a parenthesised group, and every prefix but `~` —
        // which is the same set `reassociated_unary` refuses to look at.
        // Whatever is inside one is corrected when `eval` reaches it, which is
        // why `7 DIV (2 + 1)` keeps its parentheses here as well as there.
        _ => Some(u8::MAX),
    }
}

/// `grouped` for one node, given the level of its operator.
///
/// Split out because `reassociated` has the operands in hand and not the node:
/// building one to ask about would be the clone this exists to avoid.
fn grouped_node(level: u8, left: &Expr, right: &Expr) -> Option<u8> {
    let left_level = grouped(left)?;
    let right_level = grouped(right)?;
    // Left-associative, so an operator of equal precedence belongs on the left
    // and not on the right: `1 - 2 - 3` is grouped `(1 - 2) - 3`, and
    // `7 DIV 2 * 3` arriving as `7 DIV (2 * 3)` is the same levels the wrong
    // way round and has to be rebuilt.
    (left_level >= level && right_level > level).then_some(level)
}

/// The chain's operands and operators, in the order they were written.
///
/// An operator this engine has no measured precedence for ends the chain, so a
/// comparison or an `AND` bounds it and keeps the grouping sqlparser already
/// gets right. A parenthesised group ends it too, which is why `7 DIV (2 + 1)`
/// keeps its parentheses.
///
/// A prefix `~` is corrected on the way past. It reads its operand at the
/// precedence of `+`, so `3 ^ ~1 % 5` hides the `%` inside it and the chain
/// would otherwise be two terms where it is three.
fn flatten(e: &Expr, operands: &mut Vec<Expr>, ops: &mut Vec<BinaryOperator>) {
    let corrected;
    let e = match e {
        Expr::UnaryOp { op, expr } => match reassociated_unary(op, expr) {
            Some(fixed) => {
                corrected = fixed;
                &corrected
            }
            None => e,
        },
        // A construct that extends an expression to its left is corrected on
        // the way past for the same reason: `1 XOR 2 IS NULL AND 3` hides the
        // `XOR` inside the `IS NULL`, and the chain would otherwise be two
        // terms where it is three.
        _ => match reassociated_postfix(e) {
            Some(fixed) => {
                corrected = fixed;
                &corrected
            }
            None => e,
        },
    };
    match e {
        Expr::BinaryOp { left, op, right } if precedence(op) > 0 => {
            flatten(left, operands, ops);
            ops.push(op.clone());
            flatten(right, operands, ops);
        }
        operand => operands.push(operand.clone()),
    }
}

/// Precedence climbing over the flattened chain.
fn build(
    operands: &mut std::vec::IntoIter<Expr>,
    ops: &mut std::iter::Peekable<std::vec::IntoIter<BinaryOperator>>,
    min: u8,
) -> Expr {
    // `flatten` pushes an operand either side of every operator, so there is
    // always one more of them than there are operators.
    let mut left = operands.next().expect("an operand before every operator");
    while let Some(next) = ops.peek() {
        let level = precedence(next);
        if level < min {
            break;
        }
        let op = ops.next().expect("just peeked");
        // `level + 1` rather than `level`: every operator here is
        // left-associative, so one of equal precedence belongs to the operand
        // on its left. Passing `level` would make `1 - 2 - 3` mean 2.
        let right = build(operands, ops, level + 1);
        left = Expr::BinaryOp {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
    }
    left
}

/// Re-associates the prefix `~`.
///
/// sqlparser reads its operand at the precedence of `+`, so it swallows
/// everything binding tighter than that: `~1*2` arrives as `~(1*2)`. MySQL
/// binds `~` to the operand next to it and to nothing else, which is the same
/// walk as above with a different wrapper.
pub(crate) fn reassociated_unary(op: &UnaryOperator, expr: &Expr) -> Option<Expr> {
    match op {
        UnaryOperator::BitwiseNot => {
            // Nothing to pull out of an operand that already binds tighter
            // than the prefix does, and answering that without cloning is
            // what keeps `~a & 255` off the slow path for every row.
            if grouped(expr).is_some_and(|level| level > PREFIX) {
                return None;
            }
            let mut fixed = expr.clone();
            let wrap = |operand: Expr| Expr::UnaryOp {
                op: *op,
                expr: Box::new(operand),
            };
            take_leftmost(&mut fixed, &wrap, PREFIX).then_some(fixed)
        }
        UnaryOperator::Not => reassociated_not(expr),
        _ => None,
    }
}

/// Re-associates the prefix `NOT`, which is not the same walk as `~`.
///
/// `~` binds to the operand next to it, so everything its parse swallowed has
/// to come back out and `take_leftmost` walks it all the way. `NOT` binds to a
/// whole comparison, so nearly everything sqlparser puts inside it belongs
/// there — `NOT 1 + 2` is `NOT (1 + 2)` in MySQL too. The one exception is
/// `XOR`, which is looser than `NOT` there and tighter than it in sqlparser:
/// `NOT 0 XOR 1` arrives as `NOT (0 XOR 1)` and means `(NOT 0) XOR 1`.
///
/// So this sinks rather than walks, which is `reassociated_postfix`'s step two
/// pointing the other way — into the *left* of the chain, because a prefix is
/// what the leftmost operand is missing.
fn reassociated_not(expr: &Expr) -> Option<Expr> {
    // Corrected first, because the chain it is holding need not be at the root
    // of what it holds: `NOT 2 || NULL IN (1)` arrives as
    // `NOT ((2 || NULL) IN (1))`, and only once that is
    // `NOT (2 || (NULL IN (1)))` is the `||` the operator to sink past. MySQL
    // says NULL there and the parsed shape says 0.
    let corrected;
    let operand = match corrected_shape(expr) {
        Some(fixed) => {
            corrected = fixed;
            &corrected
        }
        None => expr,
    };
    let Expr::BinaryOp { left, op, right } = operand else {
        return None;
    };
    // Zero is not a level, so an operator with none is left where it is; one
    // at `NOT`'s level or above binds at least as tightly and belongs inside.
    let held = precedence(op);
    if held == 0 || held >= NOT {
        return None;
    }
    // Left-associative, so `NOT a XOR b XOR c` is `((NOT a) XOR b) XOR c` and
    // the prefix keeps going left until it reaches the operand.
    Some(Expr::BinaryOp {
        left: Box::new(negated(left)),
        op: op.clone(),
        right: right.clone(),
    })
}

/// Whatever correction applies to a node, whichever kind of node it is.
///
/// The three correctors take different arguments because each is also called
/// from the arm that destructured its node, and a caller that has only the
/// node — which is every caller correcting somebody else's operand — wants
/// exactly this.
fn corrected_shape(e: &Expr) -> Option<Expr> {
    match e {
        Expr::BinaryOp { left, op, right } => reassociated(op, left, right),
        Expr::UnaryOp { op, expr } => reassociated_unary(op, expr),
        other => reassociated_postfix(other),
    }
}

/// `NOT` over an operand, sunk further first where the operand is itself a
/// chain looser than `NOT`.
fn negated(e: &Expr) -> Expr {
    reassociated_not(e).unwrap_or_else(|| Expr::UnaryOp {
        op: UnaryOperator::Not,
        expr: Box::new(e.clone()),
    })
}

/// How tightly a construct that extends an expression to its left binds, and
/// the operands it binds: the one it extends over, and the one after the
/// keyword where there is one.
///
/// `None` for anything else, which is what says "this is not one of them".
/// `grouped`, `take_leftmost` and `reassociated_postfix` all read the list
/// from here rather than keeping one each, because they have to agree about it
/// or a shape is refused in one place and walked in another.
///
/// `BETWEEN`'s `low` is deliberately not a trailing operand. sqlparser reads
/// it at the precedence of `BETWEEN` and so lets `XOR` into it, but MySQL will
/// not parse `1 BETWEEN 0 XOR 1 AND 2` at all — measured, it is a 1064 — so
/// there is no grouping to agree with.
///
/// `BETWEEN`'s `high` is one, and is the one place this list does not reach
/// far enough. MySQL's `high` is a `predicate`, so `1 BETWEEN 0 AND 2 IN (2)`
/// is `1 BETWEEN 0 AND (2 IN (2))` and answers 1 on 8.3; sqlparser stops the
/// `high` at `2` and makes the `IN` the parent, and this engine answers 0.
/// Correcting it would mean pushing an operator *into* a trailing operand,
/// which is the opposite of every walk here and has no other caller — so it
/// is #0091 rather than something done here, and this note is where the
/// measurement lives. Pre-existing: both constructs were refused
/// outright before #0056 and the tree was left the same way.
fn postfix_operands(e: &Expr) -> Option<(u8, &Expr, Option<&Expr>)> {
    use Expr as E;
    Some(match e {
        E::IsNull(x)
        | E::IsNotNull(x)
        | E::IsTrue(x)
        | E::IsFalse(x)
        | E::IsNotTrue(x)
        | E::IsNotFalse(x)
        | E::IsUnknown(x)
        | E::IsNotUnknown(x) => (COMPARISON, &**x, None),
        E::InList { expr, .. } | E::InSubquery { expr, .. } | E::InUnnest { expr, .. } => {
            (PREDICATE, &**expr, None)
        }
        E::Between { expr, high, .. } => (PREDICATE, &**expr, Some(&**high)),
        E::Like { expr, pattern, .. }
        | E::ILike { expr, pattern, .. }
        | E::SimilarTo { expr, pattern, .. }
        | E::RLike { expr, pattern, .. } => (PREDICATE, &**expr, Some(&**pattern)),
        // Not MySQL's spelling of anything, so there is nothing measured to
        // put it at; `COMPARISON` is where the `IS` it is written with binds.
        E::IsDistinctFrom(x, y) | E::IsNotDistinctFrom(x, y) => (COMPARISON, &**x, Some(&**y)),
        _ => return None,
    })
}

/// The same two operands, to write through.
///
/// A second copy of the list rather than one function over both: Rust has no
/// way to be generic over `&` and `&mut` here, and the alternative — cloning
/// the node to ask an immutable one — is the allocation the fast path in
/// `reassociated` exists to avoid. Anything added to one belongs in the other.
fn postfix_operands_mut(e: &mut Expr) -> Option<(u8, &mut Expr, Option<&mut Expr>)> {
    use Expr as E;
    Some(match e {
        E::IsNull(x)
        | E::IsNotNull(x)
        | E::IsTrue(x)
        | E::IsFalse(x)
        | E::IsNotTrue(x)
        | E::IsNotFalse(x)
        | E::IsUnknown(x)
        | E::IsNotUnknown(x) => (COMPARISON, &mut **x, None),
        E::InList { expr, .. } | E::InSubquery { expr, .. } | E::InUnnest { expr, .. } => {
            (PREDICATE, &mut **expr, None)
        }
        E::Between { expr, high, .. } => (PREDICATE, &mut **expr, Some(&mut **high)),
        E::Like { expr, pattern, .. }
        | E::ILike { expr, pattern, .. }
        | E::SimilarTo { expr, pattern, .. }
        | E::RLike { expr, pattern, .. } => (PREDICATE, &mut **expr, Some(&mut **pattern)),
        E::IsDistinctFrom(x, y) | E::IsNotDistinctFrom(x, y) => {
            (COMPARISON, &mut **x, Some(&mut **y))
        }
        _ => return None,
    })
}

/// The same construct over a different operand on its left.
fn with_leading(e: &Expr, operand: Expr) -> Expr {
    let mut node = e.clone();
    let (_, leading, _) =
        postfix_operands_mut(&mut node).expect("only called for one of these constructs");
    *leading = operand;
    node
}

/// The same construct with a different operand after its keyword.
fn with_trailing(e: &Expr, operand: Expr) -> Expr {
    let mut node = e.clone();
    let (_, _, trailing) =
        postfix_operands_mut(&mut node).expect("only called for one of these constructs");
    *trailing.expect("only called for one that has an operand after its keyword") = operand;
    node
}

/// Re-associates a construct that extends an expression to its left.
///
/// MySQL binds `IS NULL` where it binds a comparison, `IN`, `BETWEEN`, `LIKE`
/// and `REGEXP` one level tighter, and `XOR` far looser than any of them.
/// sqlparser puts the whole family between the comparisons and `XOR`, so each
/// is on the wrong side of one of its neighbours — and the two mistakes point
/// in opposite directions, which is why there are two corrections here.
/// Neither can be `reassociated`'s: none of these is a `BinaryOp`, so nothing
/// calls it for one.
///
/// Always answers a `BinaryOp` when it answers at all — both corrections lift
/// an operator to the root — which is what lets a caller correcting a leading
/// operand treat anything else as nothing to do.
///
/// `None` when the tree already means what MySQL means, which is what stops
/// the caller recursing forever.
pub(crate) fn reassociated_postfix(e: &Expr) -> Option<Expr> {
    let (level, leading, trailing) = postfix_operands(e)?;

    // Step one: what the operand *after* the keyword swallowed, which is the
    // same problem `DIV` has and takes the same walk. `'a' LIKE 'a' XOR 0`
    // arrives as `'a' LIKE ('a' XOR 0)` and answers 0 where MySQL says 1; so
    // does `'a' LIKE 'a' = 1`, which needs no `XOR` to go wrong and was
    // already wrong before this.
    if let Some(trailing) = trailing {
        if grouped(trailing).is_none_or(|held| held <= level) {
            let mut fixed = trailing.clone();
            let wrap = |operand: Expr| with_trailing(e, operand);
            if take_leftmost(&mut fixed, &wrap, level) {
                // Regrouped afterwards, the way `reassociated` regroups what
                // its own walk gives back. The walk only puts the construct
                // to the left of what swallowed it, and what it lands beside
                // is still grouped sqlparser's way:
                // `3 LIKE '3' <> 3 XOR NULL IS NOT NULL` comes out of the walk
                // as `(3 LIKE '3') <> (3 XOR NULL)`, where MySQL means
                // `((3 LIKE '3') <> 3) XOR ...` and answers 1 rather than 0.
                return Some(regrouped(&fixed));
            }
        }
    }

    // Step two: what the construct itself was put inside. `1 XOR 2 IS NULL`
    // arrives as `(1 XOR 2) IS NULL` and answers 0 where MySQL says 1, which
    // means `1 XOR (2 IS NULL)`.
    //
    // The leading operand is corrected first, because its root is not final
    // until it is: `1 XOR 2 | 3 IS NULL` arrives with `|` at the root of it,
    // and only once that is `1 XOR (2 | 3)` is the `XOR` the operator to sink
    // past.
    // Whichever kind of node the leading operand is, because it can be any of
    // them and each hides a chain its own way. One of these can hold another:
    // `1 XOR 2 IS NULL IS NULL` arrives as `((1 XOR 2) IS NULL) IS NULL`, and
    // the inner one has to give its `XOR` up before the outer one has a chain
    // to sink past — MySQL says 1 and reading the pair as parsed says 0. A
    // `NOT` holds one the same way: `NOT 0 XOR 1 IS NULL` arrives as
    // `NOT (0 XOR 1) IS NULL`.
    let corrected;
    let leading = match corrected_shape(leading) {
        Some(fixed) => {
            corrected = fixed;
            &corrected
        }
        None => leading,
    };
    // A leading operand that was corrected and has nothing to sink past is
    // handed back uncorrected, the way `reassociated` hands back the operands
    // it corrected and then found nothing to regroup: the caller reaches the
    // operand on its own way down and corrects it there. Both `eval` and
    // `never_null` do, and the construct itself answers the same over either
    // shape — `X IS NULL` is a LONGLONG and NOT NULL whatever `X` is grouped
    // like.
    let Expr::BinaryOp { left, op, right } = leading else {
        return None;
    };
    // Zero is not a level, so an operator with none is left where it is; one
    // at this construct's level or above binds at least as tightly as it and
    // is already inside it, which is where it belongs.
    let held = precedence(op);
    if held == 0 || held >= level {
        return None;
    }
    // And again on what it lands on, because one sink is not always enough:
    // there are four levels below a `PREDICATE`, so a corrected chain rooted
    // at `||` can still carry a `XOR` on its right. `0 || 1 XOR 2 IS NULL`
    // sinks once to `0 || ((1 XOR 2) IS NULL)`, which is still not
    // `0 || (1 XOR (2 IS NULL))`. `eval` re-enters and `flatten` finishes the
    // job either way, so this costs no answer today — but a correction that
    // is only right after being called twice is one the next caller gets
    // wrong.
    let inner = with_leading(e, (**right).clone());
    let inner = reassociated_postfix(&inner).unwrap_or(inner);
    Some(Expr::BinaryOp {
        left: left.clone(),
        op: op.clone(),
        right: Box::new(inner),
    })
}

/// Puts the corrected operator around the operand it should have taken.
///
/// Walks down the left spine of what was swallowed. Every construct here is
/// one that extends an expression to its *left* — a binary operator, a postfix
/// test, an `IN` list — so its leftmost descendant is the operand that was
/// really adjacent. A prefix or a parenthesised group is not on that list and
/// ends the walk, which is why `7 DIV (2 + 1)` keeps its parentheses.
///
/// A prefix `~` on that spine is corrected before it is stepped over, because
/// it reads its own operand at the precedence of `+` and so can be holding
/// part of the chain: `~ ~ 1 * 7` arrives as `~(~(1 * 7))`, and only correcting
/// the *outer* one first gets the `* 7` outside both of them.
///
/// `false` when the node *is* the operand, so nothing needs moving.
fn take_leftmost(e: &mut Expr, wrap: &dyn Fn(Expr) -> Expr, level: u8) -> bool {
    use Expr as E;

    if let E::UnaryOp { op, expr } = e {
        if let Some(corrected) = reassociated_unary(op, expr) {
            *e = corrected;
        }
    }

    // Something bound tighter than `op` is a unit, but it may have swallowed
    // too much of its own. Correcting it first leaves a looser operator at the
    // top, which is what the walk below can then step through; if there was
    // nothing to correct, the unit is complete and is the operand.
    if let E::BinaryOp {
        left: inner_left,
        op: inner,
        right: inner_right,
    } = e
    {
        if precedence(inner) > level {
            match reassociated(inner, inner_left, inner_right) {
                Some(corrected) => *e = corrected,
                None => return false,
            }
        }
    }

    let child: &mut Expr = match e {
        E::BinaryOp { left, .. } => left,
        _ => match postfix_operands_mut(e) {
            // Stepped into only by something that binds at least as tightly as
            // the construct does, because that is the only way one of these
            // can have been swallowed. The same test the `BinaryOp` branch
            // above makes, and a tie counts for the same reason: everything
            // here is left-associative, so a construct of equal precedence
            // belongs *outside* the one that gave up its operand.
            // `3 LIKE '2' = 3 IN (1, 1)` arrives with the `IN` inside the
            // pattern and is `(3 LIKE '2') = (3 IN (1, 1))` on 8.3.
            //
            // `AND`, `XOR` and `OR` bind looser and hold the construct as an
            // operand already, and stepping in would pull them inside it:
            // `1 AND 'a' LIKE 'a'` would become `(1 AND 'a') LIKE 'a'`, which
            // is a different question with a different answer.
            Some((held, leading, _)) if held <= level => leading,
            _ => return false,
        },
    };
    if !take_leftmost(child, wrap, level) {
        *child = wrap(child.clone());
    }
    true
}

/// `CEIL(x)` or `FLOOR(x)`, which arrive as their own node rather than a call.
///
/// The `TO <field>` and `(x, scale)` forms belong to other dialects; MySQL
/// takes one argument and nothing else, so a parse carrying either is not SQL
/// this engine should answer rather than refuse.
fn rounded_to_integer(
    name: &str,
    expr: &Expr,
    field: &sqlparser::ast::CeilFloorKind,
    ctx: &Context<'_>,
) -> EngineResult<Value> {
    use sqlparser::ast::{CeilFloorKind, DateTimeField};
    if !matches!(
        field,
        CeilFloorKind::DateTimeField(DateTimeField::NoDateTime)
    ) {
        return Err(unsupported(format!("{name} with a field or a scale")));
    }
    functions::call(name, vec![eval(expr, ctx)?], ctx.session)
}

/// MySQL's refusal when an integer result will not fit.
fn bigint_out_of_range() -> EngineError {
    EngineError::new(1690, "22003", "BIGINT value is out of range")
}

/// The same refusal, for an operator whose result type is UNSIGNED.
///
/// MySQL says which of the two it was, and the difference is the whole of
/// #0069: measured on 8.3, `iu - 100` over an `INT UNSIGNED` holding 7 raises
/// the unsigned one because the true answer -93 is below zero, while
/// `9223372036854775807 + 1` raises the signed one because the true answer is
/// above `i64::MAX`. Same code, same SQLSTATE, different domain.
fn bigint_unsigned_out_of_range() -> EngineError {
    EngineError::new(1690, "22003", "BIGINT UNSIGNED value is out of range")
}

/// The exact integer an operand contributes, or `None` if it is not one.
///
/// Only the two tags that *are* integers. A string and a float stay out on
/// purpose: MySQL reads `'7' - 100` as a double and answers -93 whatever the
/// column beside it was declared, so widening this would pull them onto the
/// exact path and change answers this issue is not about.
fn integer_operand(value: &Value) -> Option<i128> {
    match value {
        Value::Int(v) => Some(i128::from(*v)),
        Value::Unsigned(v) => Some(i128::from(*v)),
        _ => None,
    }
}

/// Whether this operator over these operands produces an UNSIGNED result.
///
/// Measured on 8.3, and the operators do not agree with each other:
///
/// | expression   | result                          |
/// |--------------|---------------------------------|
/// | `iu + 1`     | UNSIGNED — either side carries it |
/// | `100 - iu`   | UNSIGNED, and answers 93          |
/// | `iu DIV 2`   | UNSIGNED, and so is `-1 DIV iu`   |
/// | `iu MOD -1`  | UNSIGNED — but `-1 MOD iu` is not |
/// | `iu / 2`     | signed; a quotient is not an integer |
///
/// `MOD` reads only its left operand because that is the one its answer takes
/// its sign from, so an unsigned dividend can never produce a negative
/// remainder and a signed one always may.
///
/// `NO_UNSIGNED_SUBTRACTION` takes `-` out of the list and nothing else out of
/// it, which is measured rather than taken from the flag's name: under it
/// `iu - 100` is -93 and signed, while `iu + -100` and `iu * -1` still raise
/// 1690 and `iu + 1` is still UNSIGNED.
fn unsigned_result(op: &BinaryOperator, left: &Value, right: &Value) -> bool {
    use BinaryOperator as B;
    let unsigned = |v: &Value| matches!(v, Value::Unsigned(_));
    match op {
        B::Minus if crate::exec::no_unsigned_subtraction() => false,
        B::Plus | B::Minus | B::Multiply | B::MyIntegerDivide => unsigned(left) || unsigned(right),
        B::Modulo => unsigned(left),
        _ => false,
    }
}

/// An exactly-computed integer result, checked against the domain it belongs to.
///
/// The check is per operator and against the *true* value, which is what MySQL
/// does and is not what #0052 concluded for the bitwise chain: `iu - 100 + 200`
/// raises 1690 naming `(iu - 100)` on 8.3, even though 107 fits. Checking only
/// what the chain hands out would have answered 107.
///
/// `None` is out of range rather than an error of its own: the only way to get
/// one here is an `i128` multiplication overflowing, which needs both operands
/// near 2^64 and puts the answer far outside either domain.
fn fitted(result: Option<i128>, unsigned: bool) -> EngineResult<Value> {
    if unsigned {
        match result {
            Some(v) => u64::try_from(v)
                .map(Value::Unsigned)
                .map_err(|_| bigint_unsigned_out_of_range()),
            None => Err(bigint_unsigned_out_of_range()),
        }
    } else {
        match result {
            Some(v) => i64::try_from(v)
                .map(Value::Int)
                .map_err(|_| bigint_out_of_range()),
            None => Err(bigint_out_of_range()),
        }
    }
}

/// The integer behind a boolean operand, or `None` for anything else.
///
/// Shaped like `Value::temporal_number` so the two untaggings compose in
/// `arithmetic`: each hands back an owned replacement only where there is one,
/// and the borrow is kept otherwise.
fn bool_number(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(b) => Some(Value::Int(i64::from(*b))),
        _ => None,
    }
}

/// A `CASE`'s answer, with the UNSIGNED tag kept only where every branch has one.
///
/// The same all-candidates rule the eager conditionals in [`crate::functions`]
/// follow — `CASE WHEN 1 THEN iu ELSE iu END - 100` raises 1690 on 8.3 and
/// `... ELSE 1 END - 100` answers -93 — but `CASE` is the one construct that
/// does not evaluate its candidates, so the untaken branches are evaluated
/// here to be asked what they are.
///
/// Only when the answer is unsigned in the first place, which is what bounds
/// the cost: a `CASE` with nothing to lose is as lazy as it was. Where there
/// is something to lose, laziness is given up — the untaken branches are
/// evaluated once per row, and a subquery in one of them is run. That is a
/// real price and it is paid deliberately: MySQL decides this from the
/// branches' declared *types*, which are out of reach here, and evaluating
/// them is the only way to ask.
///
/// A branch that *fails* to evaluate is skipped rather than counted against
/// the tag: a branch MySQL never reaches cannot be what makes it refuse the
/// statement, and `CASE WHEN 1 THEN iu ELSE bu + 1 END` is unsigned there
/// whatever the arithmetic in the arm nobody took would have done.
fn unsigned_case(
    value: Value,
    taken: &Expr,
    conditions: &[CaseWhen],
    else_result: Option<&Expr>,
    ctx: &Context<'_>,
) -> Value {
    if !matches!(value, Value::Unsigned(_)) {
        return value;
    }
    let others = conditions
        .iter()
        .map(|w| &w.result)
        .chain(else_result)
        .filter(|other| !std::ptr::eq(*other, taken));
    for other in others {
        // A NULL branch does not count against it, which is measured: `CASE
        // WHEN 1 THEN iu END` is UNSIGNED on 8.3 with no `ELSE` at all.
        match eval(other, ctx) {
            Ok(v) if v.is_null() || matches!(v, Value::Unsigned(_)) => {}
            Ok(_) => return value.untag_unsigned(),
            Err(_) => {}
        }
    }
    value
}

/// The value an arithmetic operator produces.
///
/// `pub(crate)` because `MOD` is the `%` operator spelled as a call and has to
/// answer the same thing: typed alike and evaluated apart, `MOD(1.500, 2)`
/// promised a decimal and handed back the integer 1.
///
/// `increment` is the session's `div_precision_increment`, and only `/` reads
/// it — `DIV` and `MOD` are unaffected at every setting, measured on 8.3. It
/// arrives as a number rather than as a `Context` because that is the whole of
/// what this needs from the session, and a `Context` here would be one every
/// caller had to have.
pub(crate) fn arithmetic(
    op: &BinaryOperator,
    left: &Value,
    right: &Value,
    increment: usize,
) -> EngineResult<Value> {
    use BinaryOperator as B;

    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }

    // A boolean is 1 or 0 before any operator sees it. MySQL types `TRUE + 0`
    // and `TRUE + TRUE` LONGLONG, and without this the pair missed the integer
    // path below and came back as doubles — invisible in `SELECT`, where both
    // render `1`, and not invisible in `JSON_ARRAY(TRUE + 0)`, which would
    // have been `[1.0]` against MySQL's `[1]`.
    let (plain_left, plain_right) = (bool_number(left), bool_number(right));
    let left = plain_left.as_ref().unwrap_or(left);
    let right = plain_right.as_ref().unwrap_or(right);

    // A temporal is its packed number before any operator sees it, which is
    // the whole of #0054: MySQL reads a DATE of 2024-01-05 as 20240105 where
    // the string of those characters is 2024. Exact, so `d + 0` stays the
    // LONGLONG MySQL types it and never passes through a double.
    let (packed_left, packed_right) = (left.temporal_number(), right.temporal_number());
    let left = packed_left.as_ref().unwrap_or(left);
    let right = packed_right.as_ref().unwrap_or(right);

    // Integer arithmetic stays integral; anything else goes through f64, which
    // is what MySQL does for non-DECIMAL operands.
    //
    // Computed at `i128` and then checked, rather than at the width of the
    // answer. Neither 64-bit type can hold every operand — an `Int` reaches
    // below zero and an `Unsigned` above `i64::MAX` — and the question being
    // asked is about the *true* value: `iu - -1` is 8 and `iu - 1` is 6, both
    // answered, while `iu - 8` is refused, and all three are the same
    // subtraction over the same domain.
    //
    // `/` is the exception and goes to the exact path below: MySQL answers a
    // division with a DECIMAL even over two integers, four places wider than
    // its left operand, so `1 / 3` is 0.3333 there rather than an integer or a
    // double's worth of threes.
    if !matches!(op, B::Divide) {
        if let (Some(a), Some(b)) = (integer_operand(left), integer_operand(right)) {
            let unsigned = unsigned_result(op, left, right);
            return match op {
                B::Plus => fitted(a.checked_add(b), unsigned),
                B::Minus => fitted(a.checked_sub(b), unsigned),
                B::Multiply => fitted(a.checked_mul(b), unsigned),
                // Division by zero yields NULL in MySQL, not an error.
                B::Modulo if b == 0 => Ok(Value::Null),
                B::Modulo => fitted(Some(a % b), unsigned),
                // `DIV` truncates toward zero rather than flooring: MySQL
                // answers -3 for `-7 DIV 2`, where a floor would say -4.
                // Rust's integer division truncates the same way.
                B::MyIntegerDivide if b == 0 => Ok(Value::Null),
                B::MyIntegerDivide => fitted(Some(a / b), unsigned),
                _ => unreachable!("caller restricts the operator set"),
            };
        }
    }

    // Exact where both sides are exact, which a fractional temporal makes
    // reachable: a DATETIME(3) packs to a decimal, and `dt3 + 0` through an
    // `f64` renders 20240105060708.5 where MySQL says 20240105060708.500.
    if let Some(exact) = exact_arithmetic(op, left, right, increment) {
        return exact;
    }

    // Every non-NULL operand reads as some number, which is why there is no
    // failure case here: MySQL answers `'x' + 1` with 1, not NULL.
    let (a, b) = (left.as_number(), right.as_number());

    Ok(match op {
        B::Plus => Value::Float(a + b),
        B::Minus => Value::Float(a - b),
        B::Multiply => Value::Float(a * b),
        B::Divide if b == 0.0 => Value::Null,
        B::Divide => Value::Float(a / b),
        B::Modulo if b == 0.0 => Value::Null,
        B::Modulo => Value::Float(a % b),
        // Integral however it was reached: `7.5 DIV 2` is 3, not 3.75, and
        // MySQL types the column LONGLONG whatever it divided.
        B::MyIntegerDivide if b == 0.0 => Value::Null,
        B::MyIntegerDivide => {
            // Checked rather than cast: a float-to-integer cast saturates in
            // Rust, so `'1e400' DIV 1` would answer i64::MAX where MySQL
            // refuses the statement. Reachable from a plain string operand
            // now that a string reads as a number up to DBL_MAX.
            let quotient = (a / b).trunc();
            // A strict upper bound: `i64::MAX as f64` rounds *up* to 2^63, so
            // an inclusive range lets the one value that does not fit through,
            // and the cast then saturates to i64::MAX instead of refusing.
            if quotient < i64::MIN as f64 || quotient >= -(i64::MIN as f64) {
                return Err(bigint_out_of_range());
            }
            Value::Int(quotient as i64)
        }
        _ => unreachable!("caller restricts the operator set"),
    })
}

/// The exact answer over two exact operands, where MySQL gives one.
///
/// Every operator, and only where both sides are exact — an `Int`, an
/// `Unsigned`, or a `Decimal`. A float, a string or a JSON document on either
/// side makes MySQL compute in a double and the caller falls through to one.
///
/// The scales are MySQL's, measured on 8.3 over a `DECIMAL(10,2)` and a
/// `DECIMAL(8,3)` both holding 1.5:
///
/// | operator | scale | over 1.5 and 1.5 |
/// |---|---|---|
/// | `+`, `-` | the wider of the two | 3.000, 0.000 |
/// | `*` | the two added | 2.25000 |
/// | `/` | the left one plus `increment` | 1.000000 |
/// | `%`, `MOD` | the wider of the two | 0.000 |
/// | `DIV` | none — a BIGINT whatever it divided | 1 |
///
/// A zero divisor is NULL rather than an error for all three of `/`, `%` and
/// `DIV`, which is what a `SELECT` gets on 8.3 under every `sql_mode`
/// including strict; the 1365 that `ERROR_FOR_DIVISION_BY_ZERO` names belongs
/// to a statement that would *store* the answer.
///
/// `None` means "not this path's to answer", and the caller computes in an
/// `f64`. An answer of `Some` is final, error included: `DIV` is the one
/// operator here that can refuse, because its answer is a BIGINT and
/// `99999999999999999999999.0 DIV 1` is 1690 on 8.3.
fn exact_arithmetic(
    op: &BinaryOperator,
    left: &Value,
    right: &Value,
    increment: usize,
) -> Option<EngineResult<Value>> {
    use BinaryOperator as B;
    // An unsigned operand is exact too, and it is not range-checked here: a
    // decimal result is not an integer one, so MySQL answers `iu - 100.5`
    // -93.5 rather than refusing it, and `bu + 1.5` is 18446744073709551616.5
    // — past `u64::MAX` and still an answer.
    let (a, b) = (left.exact_decimal()?, right.exact_decimal()?);
    if matches!(op, B::Divide | B::Modulo | B::MyIntegerDivide) && crate::decimal::is_zero(&b) {
        return Some(Ok(Value::Null));
    }
    let text = match op {
        B::Plus => crate::decimal::add(&a, &b),
        B::Minus => crate::decimal::subtract(&a, &b),
        B::Multiply => crate::decimal::multiply(&a, &b),
        B::Divide => crate::decimal::divide(&a, &b, increment),
        B::Modulo => crate::decimal::remainder(&a, &b),
        B::MyIntegerDivide => {
            let quotient = crate::decimal::integer_divide(&a, &b)?;
            // Through `fitted`, so the answer lands in the same domain the
            // integer path above would have put it in: MySQL keeps the
            // UNSIGNED tag on `DIV` from either operand, and `bu DIV 1.0` is
            // 18446744073709551615 there exactly as `bu DIV 1` is. Refusing
            // the first because a decimal divisor took it off the integer
            // path would make the answer depend on how the 1 was written.
            //
            // A quotient too long for an `i128` parses as `None`, which
            // `fitted` already reads as out of range — the only way to get one
            // is a numerator past 38 digits, which no BIGINT of either sign
            // could hold either.
            return Some(fitted(
                quotient.parse::<i128>().ok(),
                unsigned_result(op, left, right),
            ));
        }
        _ => return None,
    };
    Some(Ok(Value::Decimal(text?)))
}

/// A bitwise chain's answer as a `Value`.
///
/// MySQL answers every bitwise operator as a `BIGINT UNSIGNED`, and while this
/// engine held only an `i64` an answer above `i64::MAX` had nothing to be, so
/// `SELECT ~0` and `SELECT 1 << 63` were refused with 1235. That was #0077,
/// and it closed when #0069 gave `Value` an unsigned tag for its own reasons.
///
/// Keeping the bits in an `i64` and letting the column's UNSIGNED flag
/// reinterpret them was the alternative, and it is why the refusal stood as
/// long as it did: it looks like it works for a value that leaves as a column
/// of its own and is never read again, and *every* other reader sees the raw
/// `i64` with nothing on the value to tell it otherwise. Measured against 8.3,
/// that answered `~0 MOD 10` 18446744073709551615 for 5, `CONCAT(~0)` -1,
/// `~0 > 5` 0 for 1, and a `UNION` dropped the flag and printed -1. The tag is
/// what tells them.
///
/// One reader it does *not* fix, and it is named here rather than left to be
/// discovered: `/`. MySQL answers `~0 / 2` 9223372036854775807.5000 and this
/// engine answers 9223372036854776000, because a quotient goes through an
/// `f64` whatever its operands were tagged. That is not the unsigned tag
/// missing — `9223372036854775807 / 2` is 4611686018427388000 here for the
/// same reason, and has been since long before the tag existed. It is #0085,
/// and `exact_arithmetic` above says why `/` is left out of the exact path:
/// MySQL widens a quotient by `div_precision_increment` and this engine does
/// not, so answering half of that rule exactly would make the gap depend on
/// what was divided. So this is a wrong answer rather than a refusal, in one
/// operator, knowingly, and it is the same wrong answer a signed operand of
/// that size already got.
fn bit_pattern(bits: u64) -> EngineResult<Value> {
    Ok(Value::Unsigned(bits))
}

/// The bit pattern MySQL reads out of one operand.
///
/// Not `as_number`, because MySQL reads a string and a number differently and
/// answering both the same way gets one of them wrong: a *string* converts by
/// its leading integer, so `'3.7' | 0` is 3 and `'1e2' | 0` is 1, where a
/// number is rounded to the nearest integer first, so `3.7 & 1` is 0 and
/// `-2.5 & 3` is 1.
///
/// A binary string is refused rather than converted. MySQL gives these
/// operators a second meaning over one — `_binary'ab' & _binary'cd'` is a
/// bytewise AND of two strings, and operands of unequal length are refused
/// with 3513 — and that meaning is not implemented. The engine cannot answer
/// the *numeric* readings either, because `Value::Bytes` is what both a
/// `VARBINARY` column and a hex literal come back as, and MySQL reads those
/// two oppositely: `_binary'ab' & 1` is 0, reading the bytes as the characters
/// `ab`, while `X'0F' & 255` is 15, reading them as a number. One `Value` and
/// two answers, so neither is given.
///
/// A temporal converts to its packed digits, which is where the two readings
/// differ: measured on 8.3, a DATE of 2026-01-02 is 20260102, a DATETIME
/// 20260102030405 and a TIME of 06:07:08 is 60708, where a *string* of those
/// same characters converts by its leading integer and gives 2026 and 6. Until
/// #0054 the engine stored a temporal as `Value::Text` and the two arrived
/// here identical, so this refused rather than guessing; the tag is what
/// tells them apart now.
fn bit_operand(v: &Value) -> EngineResult<u64> {
    Ok(match v {
        Value::Int(n) => *n as u64,
        // Already the bit pattern, and it has to be read as one: through the
        // `f64` fall-through below, `0 | 18446744073709551615` came back
        // 9223372036854775807 because the cast saturates.
        Value::Unsigned(n) => *n,
        // Through the same packed number arithmetic uses, so `d | 0` and
        // `d + 0` cannot disagree about what a DATE is worth. A fractional
        // temporal packs to a decimal and is rounded like any other decimal
        // operand — `dt3 | 0` is 20240105060709 for 06:07:08.500.
        Value::Temporal(text) => match v.temporal_number() {
            Some(Value::Int(n)) => n as u64,
            Some(Value::Decimal(number)) => decimal_bits(&number),
            // Text a DATE column holds that is not a temporal at all, which
            // nothing validates on the way in. Read as the string it is,
            // which is what it was before the tag existed.
            _ => integer_prefix(text),
        },
        // Exact rather than through an `f64`, because the rounding is a tie
        // rule and a double loses ties past the fifteenth digit:
        // `DECIMAL(20,1)` holding 4503599627370496.5 is 4503599627370497 in
        // MySQL and 4503599627370496 through a double, which lands on the
        // wrong side of every bit it is then masked with.
        Value::Decimal(text) => decimal_bits(text),
        Value::Text(s) | Value::Json(s) => integer_prefix(s),
        // Its ordinal, through the same number arithmetic uses, so `e | 0` and
        // `e + 0` cannot disagree about what a member is worth. It is a whole
        // number already, so neither reading below would round it.
        Value::Enum { ordinal, .. } => u64::from(*ordinal),
        Value::Bytes(_) => {
            return Err(unsupported("a bitwise operator over a binary string"));
        }
        // Away from zero at a half, which is MySQL's rounding and Rust's:
        // `2.5 & 3` is 3 and `-2.5 & 3` is 1. The cast saturates rather than
        // wrapping, which is also MySQL's answer — `1e30 | 0` is
        // 9223372036854775807 and not the unsigned maximum.
        other => other.as_number().round() as i64 as u64,
    })
}

/// The integer at the front of a string, as MySQL converts one.
///
/// Stops at the first character that is not a digit, so `'3.7'` is 3, `'1e2'`
/// is 1 and `'x'` is 0 — the point and the exponent that `as_number` reads are
/// not part of an integer. The two ends clamp differently, which is measured
/// rather than reasoned: `'99999999999999999999' | 0` is the unsigned maximum
/// and `'-99999999999999999999' | 0` is the *signed* minimum.
fn integer_prefix(s: &str) -> u64 {
    let text = s.trim_start();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let digits = &digits[..end];
    if digits.is_empty() {
        return 0;
    }
    if negative {
        // Parsed without the sign and negated after, so the clamp lands on
        // `i64::MIN` rather than on whatever `"-…"` failed to parse as.
        digits.parse::<i64>().map_or(i64::MIN as u64, |n| -n as u64)
    } else {
        digits.parse::<u64>().unwrap_or(u64::MAX)
    }
}

/// The integer MySQL reads out of an exact decimal.
///
/// Rounded to an integer away from zero at a half, which is the rule #0052
/// measured for a *number* operand — `2.5 | 0` is 3 and `-2.5 | 0` is -3 — but
/// applied to the digits rather than to a double, so a decimal wider than an
/// `f64`'s fifteen significant digits keeps its tie.
///
/// Saturating at the *signed* range, which is measured and is not what a
/// string does: a `DECIMAL(30,0)` of thirty digits reads 9223372036854775807
/// and its negative reads back the bits of `i64::MIN`, where
/// `'99999999999999999999' | 0` is the unsigned maximum.
///
/// That saturation is MySQL's for a *signed* decimal only. An UNSIGNED one is
/// read through MySQL's unsigned path and keeps its value —
/// `DECIMAL(30,0) UNSIGNED` holding 12345678901234567890 answers that number,
/// and `& 255` answers 210 where this answers 255. `Value::Decimal` carries
/// the digits and not the column's sign, so the two are the same value here:
/// the same defect as #0054 and #0068, in the one flag rather than the type.
fn decimal_bits(text: &str) -> u64 {
    // `units` at scale zero *is* the rounding: it carries the first dropped
    // digit into the count, away from zero, which is MySQL's rule for both.
    let Some(units) = crate::decimal::units(text, 0) else {
        // Not a plain decimal — more digits than an `i128` counts, or an
        // exponent. The float reading saturates to the same two ends.
        return crate::value::numeric_prefix(text).round() as i64 as u64;
    };
    let clamped = if units < 0 { i64::MIN } else { i64::MAX };
    i64::try_from(units).unwrap_or(clamped) as u64
}

/// Whether an operator computes over bits rather than over numbers.
fn bitwise_op(op: &BinaryOperator) -> bool {
    use BinaryOperator as B;
    matches!(
        op,
        B::BitwiseAnd
            | B::BitwiseOr
            | B::BitwiseXor
            | B::PGBitwiseShiftLeft
            | B::PGBitwiseShiftRight
    )
}

/// The value a bitwise chain produces.
///
/// Range-checked exactly once, here, where the chain hands its answer to
/// something that is not another bitwise operator. Checking every operator
/// would refuse far more than it has to: `1 + 3 & ~5` is 0 in MySQL and
/// reaches it through 18446744073709551610, a number this engine cannot hold
/// but can carry the bits of.
fn bitwise_value(e: &Expr, ctx: &Context<'_>) -> EngineResult<Value> {
    match bitwise_bits(e, ctx)? {
        Some(bits) => bit_pattern(bits),
        None => Ok(Value::Null),
    }
}

/// The 64 bits a bitwise chain computes, or `None` where it is NULL.
///
/// Recurses through the chain rather than going back out through `eval`, so
/// that a chain is computed as a chain. Every operator here is exact on the
/// bits whatever their sign, so nothing is lost between them — which mattered
/// more when the boundary was the only place a `Value` could be built, and is
/// still what makes `1 + 3 & ~5` reach 0 through 18446744073709551610.
fn bitwise_bits(e: &Expr, ctx: &Context<'_>) -> EngineResult<Option<u64>> {
    use BinaryOperator as B;
    match e {
        // Transparent: parentheses group the chain, they do not end it.
        // `(~0) & 255` is one chain and answers 255.
        Expr::Nested(inner) => bitwise_bits(inner, ctx),
        Expr::BinaryOp { left, op, right } => {
            if let Some(fixed) = reassociated(op, left, right) {
                return bitwise_bits(&fixed, ctx);
            }
            if !bitwise_op(op) {
                return operand_bits(e, ctx);
            }
            let (Some(a), Some(b)) = (bitwise_bits(left, ctx)?, bitwise_bits(right, ctx)?) else {
                return Ok(None);
            };
            Ok(Some(match op {
                B::BitwiseAnd => a & b,
                B::BitwiseOr => a | b,
                B::BitwiseXor => a ^ b,
                // The shift count is read unsigned and is not reduced modulo
                // anything: `1 << 64` is 0 and so is `1 << -1`, where -1
                // arrives as a count of 18446744073709551615.
                B::PGBitwiseShiftLeft if b >= 64 => 0,
                B::PGBitwiseShiftLeft => a << b,
                B::PGBitwiseShiftRight if b >= 64 => 0,
                // Logical, not arithmetic: the operand is unsigned, so
                // `-1 >> 1` is 9223372036854775807 rather than -1.
                B::PGBitwiseShiftRight => a >> b,
                _ => unreachable!("guarded by `bitwise_op`"),
            }))
        }
        Expr::UnaryOp { op, expr } => {
            if let Some(fixed) = reassociated_unary(op, expr) {
                return bitwise_bits(&fixed, ctx);
            }
            if !matches!(op, UnaryOperator::BitwiseNot) {
                return operand_bits(e, ctx);
            }
            Ok(bitwise_bits(expr, ctx)?.map(|bits| !bits))
        }
        other => operand_bits(other, ctx),
    }
}

/// One operand of a chain: everything the chain is not.
///
/// Evaluated as an ordinary expression and then read as bits, which is where
/// an operand that cannot be read as bits at all — a binary string — is
/// refused.
fn operand_bits(e: &Expr, ctx: &Context<'_>) -> EngineResult<Option<u64>> {
    // A literal with a point used to be read off its *token* here, because
    // the token said DECIMAL where the value it evaluated to was an `f64` and
    // had lost the tie at the seventeenth digit. #0070 made that literal a
    // `Decimal`, so the value now carries the same type the token did and
    // `bit_operand` reads it exactly — including through a sign, which folded
    // into the token by hand and folds into the value by arithmetic.
    let value = eval(e, ctx)?;
    if value.is_null() {
        return Ok(None);
    }
    bit_operand(&value).map(Some)
}

/// The field `EXTRACT(<field> FROM x)` names.
///
/// Two families, and the split is not cosmetic: MySQL converts the argument to
/// a DATETIME for the calendar fields and to a TIME for the clock ones, and
/// the two read the same value differently. Over a TIME column
/// `EXTRACT(DAY FROM t)` is a day of *today's* month, while
/// `EXTRACT(HOUR FROM t)` is the hours the TIME holds, sign and all.
#[derive(Clone, Copy)]
enum ExtractField {
    Year,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Microsecond,
}

impl ExtractField {
    /// Whether MySQL reads the argument as a DATETIME rather than as a TIME.
    fn calendar(self) -> bool {
        matches!(
            self,
            ExtractField::Year
                | ExtractField::Quarter
                | ExtractField::Month
                | ExtractField::Week
                | ExtractField::Day
        )
    }
}

/// `EXTRACT(YEAR FROM x)`, which sqlparser gives its own node rather than
/// routing through `Expr::Function` — which is why it reached the evaluator's
/// catch-all and answered 1235 for every unit.
///
/// MySQL takes twenty units here, measured against 8.3 by asking it each in
/// turn: the nine below and eleven compounds — `YEAR_MONTH`, `DAY_HOUR`,
/// `DAY_MINUTE`, `DAY_SECOND`, `DAY_MICROSECOND`, `HOUR_MINUTE`,
/// `HOUR_SECOND`, `HOUR_MICROSECOND`, `MINUTE_SECOND`, `MINUTE_MICROSECOND`
/// and `SECOND_MICROSECOND` — which pack the fields they name into one number
/// at two digits each and six for the microseconds, so `DAY_SECOND` over
/// `2024-01-05 06:07:08` is 5060708. Those eleven do not arrive here at all:
/// sqlparser has no keyword for them and its MySQL dialect refuses the
/// `Custom` spelling, so the statement fails to parse. It is exactly the same
/// set MySQL accepts after `INTERVAL`, which was measured rather than assumed
/// — neither side takes a unit the other refuses.
///
/// Every unit sqlparser *does* recognise but MySQL does not — `DOW`, `EPOCH`,
/// `ISOYEAR`, the plural forms — is refused here rather than answered, since
/// MySQL calls each of them a syntax error.
fn extract(field: &DateTimeField, arg: &Expr, ctx: &Context<'_>) -> EngineResult<Value> {
    use crate::temporal;

    let unit = match field {
        DateTimeField::Year => ExtractField::Year,
        DateTimeField::Quarter => ExtractField::Quarter,
        DateTimeField::Month => ExtractField::Month,
        DateTimeField::Week(None) => ExtractField::Week,
        DateTimeField::Day => ExtractField::Day,
        DateTimeField::Hour => ExtractField::Hour,
        DateTimeField::Minute => ExtractField::Minute,
        DateTimeField::Second => ExtractField::Second,
        DateTimeField::Microsecond => ExtractField::Microsecond,
        other => return Err(unsupported(format!("the `{other}` unit in EXTRACT()"))),
    };

    let value = eval(arg, ctx)?;
    if value.is_null() {
        return Ok(Value::Null);
    }
    let text = value.as_text();
    // Whether the value carries a temporal *type* or is text that merely looks
    // like one. They are read by different rules and give different answers,
    // which is the distinction `Value::Temporal` exists to make: MySQL reads
    // `EXTRACT(SECOND FROM d)` over a DATE column as 0 and
    // `EXTRACT(SECOND FROM '2024-01-05')` as 24, because the string is read as
    // the *time* 00:20:24.
    let typed = matches!(value, Value::Temporal(_));

    let (negative, stamp) = if unit.calendar() {
        match temporal::parse(&text) {
            Some(stamp) => (false, stamp),
            // A TIME has no calendar of its own, so MySQL puts it on today's:
            // `EXTRACT(DAY FROM t)` over a TIME column holding `-06:07:08` is
            // yesterday until the clock passes six in the morning. Only for a
            // typed temporal — the same characters as a *string* are read as a
            // date there, `'06:07:08'` giving 2006-07-08, and this engine's
            // parser does not read that shape.
            None if typed => {
                let Some((negative, clock)) = temporal::parse_signed_clock(&text) else {
                    return Ok(Value::Null);
                };
                let Some(today) = temporal::parse(&functions::now_datetime()[..10]) else {
                    return Ok(Value::Null);
                };
                // Through microseconds rather than seconds, because a negative
                // TIME of `-00:00:00.5` crosses midnight on the fraction alone
                // and a second-wide step would leave it on today.
                let micros = i64::from(clock.hour) * 3_600_000_000
                    + i64::from(clock.minute) * 60_000_000
                    + i64::from(clock.second) * 1_000_000
                    + i64::from(clock.micros);
                match today.add(
                    temporal::Unit::Microsecond,
                    if negative { -micros } else { micros },
                ) {
                    Some(stamp) => (false, stamp),
                    None => return Ok(Value::Null),
                }
            }
            None => return Err(unreadable(&text)),
        }
    } else {
        // MySQL reads a string as a TIME first and only tries a datetime when
        // it is at least twelve characters long — and its datetime reader does
        // not take the `T` separator on this path, which the engine's does.
        // Both measured: `EXTRACT(SECOND FROM '2024-01-05')` is 24 and
        // `EXTRACT(SECOND FROM '2024-01-05T06:07:08')` is 24 as well, where
        // the space-separated spelling of the same instant is 8.
        //
        // Both halves read the *trimmed* text, and that is the whole guard:
        // measuring the length after trimming while looking for the space
        // before it let surrounding whitespace answer the space test on its
        // own, so `'2024-01-05T06:07:08 '` passed a guard written to stop
        // `'2024-01-05T06:07:08'` — and `temporal::parse` trims and takes the
        // `T`, so it answered 8 where MySQL answers 24.
        //
        // Trimming is the right reading for the length too, though not
        // exactly: measured on 8.3, leading whitespace does *not* count toward
        // the twelve — `' 4-1-5 6:7:8'` is 4, the TIME reading, at a raw
        // length of 12 — while a trailing space does, so `'4-1-5 6:7:8 '` is
        // 8. Trimming agrees with MySQL on the first and refuses the second
        // rather than answering it, which is the safe direction.
        let trimmed = text.trim();
        let looks_like_a_datetime = typed || (trimmed.len() >= 12 && trimmed.contains(' '));
        match looks_like_a_datetime
            .then(|| temporal::parse(trimmed))
            .flatten()
        {
            Some(stamp) => (false, stamp),
            None => match temporal::parse_signed_clock(&text) {
                Some(read) => read,
                // A typed temporal that is neither is a column holding text
                // nothing validated on the way in; MySQL has no such state and
                // NULL is what the engine answers for it elsewhere.
                None if typed => return Ok(Value::Null),
                None => return Err(unreadable(&text)),
            },
        }
    };

    let read = match unit {
        ExtractField::Year => stamp.year,
        ExtractField::Quarter => i64::from((stamp.month - 1) / 3 + 1),
        ExtractField::Month => i64::from(stamp.month),
        // The one field with a mode. `EXTRACT` takes no argument for it and
        // reads `default_week_format` instead, which is 0 on a stock 8.3 and
        // moves 2024-01-05 from week 0 to week 1 when set to 1.
        //
        // Through the session's own accessor rather than `system_variable`,
        // which normalises the name into a fresh `String` and clones the value
        // it finds: this runs once per row, and #0078 is about exactly that
        // cost paid per row for something that cannot change mid-statement.
        ExtractField::Week => i64::from(stamp.week(ctx.session.default_week_format())),
        ExtractField::Day => i64::from(stamp.day),
        ExtractField::Hour => i64::from(stamp.hour),
        ExtractField::Minute => i64::from(stamp.minute),
        ExtractField::Second => i64::from(stamp.second),
        ExtractField::Microsecond => i64::from(stamp.micros),
    };
    // The sign rides on every clock field and on none of the calendar ones,
    // which is measured: over a TIME of `-06:07:08`, `EXTRACT(HOUR ...)` is -6
    // where `HOUR()` over the same value is 6, and `EXTRACT(YEAR ...)` is this
    // year rather than minus it.
    Ok(Value::Int(if negative { -read } else { read }))
}

/// Text `EXTRACT` will not guess at.
///
/// MySQL answers NULL for text it cannot read as a temporal at all, and a
/// number for several shapes this engine's parser does not take — `'06:07:08'`
/// is the date 2006-07-08 to a calendar field, and a bare `20240105` is one
/// too. Answering NULL for those would be a wrong answer wearing MySQL's
/// clothes, so the ones that cannot be told apart are refused together.
fn unreadable(text: &str) -> EngineError {
    unsupported(format!(
        "EXTRACT() over `{text}`, which is not a date or time this engine reads"
    ))
}

/// The date functions whose arguments are partly syntax.
///
/// `DATE_ADD(x, INTERVAL 1 DAY)` carries an interval, and
/// `TIMESTAMPDIFF(DAY, a, b)` names its unit with a bare keyword that would
/// otherwise resolve as a column — which is exactly the error it used to give.
fn temporal_call(
    name: &str,
    f: &sqlparser::ast::Function,
    ctx: &Context<'_>,
) -> EngineResult<Value> {
    use crate::temporal::{self, Unit};

    let args = argument_exprs(f);
    // NULL in, NULL out — and a value MySQL cannot read as a datetime is NULL
    // too, with a warning rather than a failed statement.
    // The argument's precision as well as its stamp. MySQL gives the shifted
    // value the precision of the argument's *declared type*, which is measured
    // and not the same as the digits it was written with: `DATE_ADD(dt3,
    // INTERVAL 1 SECOND)` over a DATETIME(3) keeps three digits, while
    // `DATE_ADD('2026-01-01 00:00:00.1', INTERVAL 1 SECOND)` over a string —
    // which has no declared precision — is padded to `.100000`.
    //
    // Which is another thing only the tag can tell apart: a `Temporal` holds
    // exactly the digits its column or cast declared, and anything else holds
    // characters that declare nothing.
    let read = |e: &Expr| -> EngineResult<Option<(temporal::Stamp, u32)>> {
        let value = eval(e, ctx)?;
        if value.is_null() {
            return Ok(None);
        }
        let text = value.as_text();
        let precision = match &value {
            Value::Temporal(declared) => temporal::fraction_digits(declared),
            _ if temporal::fraction_digits(&text) > 0 => 6,
            _ => 0,
        };
        Ok(temporal::parse(&text).map(|stamp| (stamp, precision)))
    };
    let stamp = |e: &Expr| -> EngineResult<Option<temporal::Stamp>> {
        Ok(read(e)?.map(|(stamp, _)| stamp))
    };
    // Back into the shape it arrived in, at the precision it arrived with.
    let shifted = |stamp: temporal::Stamp, precision: u32| -> Value {
        let shape = if stamp.has_time {
            temporal::Shape::DateTime
        } else {
            temporal::Shape::Date
        };
        let rendered = stamp.render();
        Value::Temporal(temporal::normalise(&rendered, shape, precision).unwrap_or(rendered))
    };

    match name {
        "timestampdiff" | "timestampadd" => {
            let [unit, first, second] = args.as_slice() else {
                return Err(unsupported(format!("{name}() with these arguments")));
            };
            // The unit arrives as an identifier because that is what a bare
            // keyword parses to; its *name* is the value.
            let Expr::Identifier(ident) = unit else {
                return Err(unsupported(format!("a computed unit in {name}()")));
            };
            let unit = Unit::parse(&ident.value)?;

            if name == "timestampadd" {
                let Some((to, precision)) = read(second)? else {
                    return Ok(Value::Null);
                };
                let Some(amount) = eval(first, ctx)?.as_int() else {
                    return Ok(Value::Null);
                };
                return Ok(match to.add(unit, amount) {
                    Some(moved) => shifted(moved, precision),
                    None => Value::Null,
                });
            }

            match (stamp(first)?, stamp(second)?) {
                (Some(from), Some(to)) => Ok(from.diff(unit, &to).map_or(Value::Null, Value::Int)),
                _ => Ok(Value::Null),
            }
        }
        _ => {
            let [subject, interval] = args.as_slice() else {
                return Err(unsupported(format!("{name}() with these arguments")));
            };
            let Some((stamp, precision)) = read(subject)? else {
                return Ok(Value::Null);
            };

            let Some((amount, unit)) = read_interval(interval, ctx)? else {
                return Ok(Value::Null);
            };
            // `checked_neg`, because negating `i64::MIN` overflows and
            // `DATE_SUB(x, INTERVAL -9223372036854775808 DAY)` is a statement
            // anyone can write.
            let signed = if name == "date_sub" || name == "subdate" {
                match amount.checked_neg() {
                    Some(signed) => signed,
                    None => return Ok(Value::Null),
                }
            } else {
                amount
            };
            // Tagged whatever it shifted, which is measured rather than
            // inferred from the metadata: MySQL types
            // `DATE_ADD('2024-01-05', INTERVAL 1 DAY)` a *string* and still
            // reads it 20240106 in arithmetic, where the bare literal reads
            // 2024. The number follows the value, not the declared type.
            Ok(match stamp.add(unit, signed) {
                Some(moved) => shifted(moved, precision),
                None => Value::Null,
            })
        }
    }
}

/// The amount and unit of an `INTERVAL n UNIT`.
///
/// `ADDDATE(x, 7)` — a bare number with no interval — means days, which is the
/// one place MySQL infers a unit.
fn read_interval(
    e: &Expr,
    ctx: &Context<'_>,
) -> EngineResult<Option<(i64, crate::temporal::Unit)>> {
    use crate::temporal::Unit;

    // NULL is a value here, not a failure: `DATE_SUB(NOW(), INTERVAL ? DAY)`
    // with a NULL parameter is NULL in MySQL, and erroring would fail the
    // whole statement over one unbound row.
    //
    // A fractional amount rounds rather than truncating, which is what MySQL
    // does — `INTERVAL 1.5 DAY` moves two days, not one.
    let amount = |e: &Expr| -> EngineResult<Option<i64>> {
        let value = eval(e, ctx)?;
        if value.is_null() {
            return Ok(None);
        }
        // Via `as_float`, because `as_int` truncates a float and MySQL rounds:
        // `INTERVAL 1.5 DAY` moves two days, not one. An out-of-range float
        // saturates rather than wrapping, and the checked arithmetic in
        // `Stamp::add` turns that into NULL.
        Ok(match value.as_float() {
            Some(number) => Some(number.round() as i64),
            None => value.as_int(),
        })
    };

    let Expr::Interval(interval) = e else {
        return Ok(amount(e)?.map(|a| (a, Unit::Day)));
    };

    let Some(field) = &interval.leading_field else {
        return Err(unsupported("an INTERVAL without a unit"));
    };
    let unit = Unit::parse(&field.to_string())?;
    Ok(amount(&interval.value)?.map(|a| (a, unit)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    /// Counts allocations, because taking the fast path is not observable any
    /// other way.
    ///
    /// `reassociated` answers `None` for a chain that is already grouped
    /// MySQL's way whether or not `grouped_node` recognised it first — the
    /// long way round arrives at the same answer, which is the whole reason
    /// the waste was silent. What separates them is that the long way clones
    /// both operands and rebuilds the tree, so a test that counts allocations
    /// is a test that fails when the early exit stops firing.
    ///
    /// Per thread rather than per process: the test binary runs its tests in
    /// parallel, and a shared counter would be measuring whatever else happens
    /// to be running.
    struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
            // SAFETY: the layout is the caller's, passed through unchanged.
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: as above, and the pointer came from `System.alloc`.
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    fn parse(sql: &str) -> Expr {
        sqlparser::parser::Parser::new(&sqlparser::dialect::MySqlDialect {})
            .try_with_sql(sql)
            .and_then(|mut p| p.parse_expr())
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    /// The shapes an application writes, none of which needs a thing done to
    /// it: sqlparser and MySQL group `+ - * / %` identically, and every row of
    /// every query was rebuilding them anyway.
    #[test]
    fn a_chain_mysql_already_agrees_with_is_not_rebuilt() {
        for sql in [
            "a + b",
            "a * b + c",
            "a + b * c",
            "a - b - c",
            "price * quantity + shipping - discount",
            "a + b * c - d / e % f",
            "(a + b) * c",
            "a * (b + c)",
            "~a & 255",
            // The shapes a `WHERE` is made of. #0056 gave every comparison,
            // `AND` and `OR` a level, which put them into the chain for the
            // first time; without the fast path recognising them, every row of
            // every filtered query would clone and rebuild its predicate.
            "a = 1",
            "a = 1 AND b = 2",
            "a = 1 AND b = 2 AND c = 3",
            "a = 1 OR b = 2",
            "a = 1 AND b > 2 OR c <= 3 AND d <> 4",
            "a IS NULL AND b = 2",
            "a IN (1, 2) AND b IS NOT NULL",
            "a LIKE 'x%' AND b BETWEEN 1 AND 2",
            "a + 1 = b * 2",
            "a XOR b",
            // A `NOT` whose operand binds tighter than it does, which is
            // every `NOT` an application writes.
            "NOT a AND NOT b",
            "NOT a = 1 AND b",
            "NOT a IS NULL OR b",
        ] {
            let expr = parse(sql);
            let Expr::BinaryOp { left, op, right } = &expr else {
                panic!("{sql} did not parse to a binary operator");
            };
            let before = ALLOCATIONS.with(Cell::get);
            let fixed = reassociated(op, left, right);
            let allocated = ALLOCATIONS.with(Cell::get) - before;
            assert!(fixed.is_none(), "`{sql}` was regrouped");
            assert_eq!(allocated, 0, "`{sql}` allocated {allocated} times");
        }
    }

    /// The other half of the same claim: a shape that *is* grouped wrongly
    /// must not be recognised as already grouped, or the correction stops
    /// happening. These are the answers `crate::exec`'s tests pin.
    #[test]
    fn a_chain_that_needs_correcting_is_not_taken_for_a_grouped_one() {
        for sql in [
            // `20 DIV (2 * 5)`: the same level to the right of DIV, which is
            // left-associative and so belongs on its left.
            "20 DIV 2 * 5",
            // `(1 + 2) ^ 3`: a looser operator to the left of `^`, which
            // sqlparser puts below `+` and MySQL puts above it.
            "1 + 2 ^ 3",
            // `7 DIV (2 IS NULL)`: not an operator at all, and only the walk
            // in `take_leftmost` puts it back.
            "7 DIV 2 IS NULL",
            // `~((-6) * 2) ^ 3` and `3 ^ ~(1 % 5)`: a prefix holding part of
            // the chain, on either side of the operator.
            "~(-6) * 2 ^ 3",
            "3 ^ ~1 % 5",
            // `(2 XOR 3) AND 0`: sqlparser puts XOR eight levels above where
            // MySQL puts it, so everything between the two arrives inside it.
            "2 XOR 3 AND 0",
            "1 XOR 2 | 0",
            "1 XOR 2 = 2",
            // And the same disagreement read from the other side, where the
            // XOR is what got swallowed: `1 = (2 XOR 3)`.
            "1 = 2 XOR 3",
            // `(1 || 1) AND 0`: sqlparser reads `||` at the precedence of `*`
            // whichever of the two operators it means, and meaning OR it is
            // the loosest thing in the statement.
            "1 || 1 AND 0",
            // `(a AND (b XOR c)) OR d`: three of the four new levels in one
            // chain, and the only reading of it sqlparser gets right is which
            // of them is loosest.
            "a AND b XOR c OR d",
            // `NOT (0 XOR 1) AND 0`: sqlparser lets the whole XOR chain into
            // the `NOT`, where MySQL binds it to the operand beside it.
            "NOT 0 XOR 1 AND 0",
        ] {
            let expr = parse(sql);
            let Expr::BinaryOp { left, op, right } = &expr else {
                panic!("{sql} did not parse to a binary operator");
            };
            assert!(
                grouped_node(precedence(op), left, right).is_none(),
                "`{sql}` was taken for a chain that needs nothing"
            );
        }
    }

    /// `take_leftmost` steps into a construct like `LIKE` only on behalf of an
    /// operator that binds tighter than a comparison, because that is the only
    /// way one can have been swallowed.
    ///
    /// Asserted here rather than through a query, and deliberately: the guard
    /// is inert as the rest of the file stands, because a looser operator
    /// walked wrongly *into* the construct is sunk straight back out of it by
    /// `reassociated_postfix`, so no answer changes and no mutation of it
    /// fails a test. What the guard buys is that `take_leftmost` is right on
    /// its own terms rather than by something downstream repairing it — the
    /// two exist to correct opposite mistakes, and a pass that makes one to
    /// have the other undone is a pass nobody can reason about.
    #[test]
    fn a_postfix_construct_is_walked_into_only_from_something_tighter() {
        let like = parse("'a' LIKE 'b'");
        let wrap = |operand: Expr| Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr: Box::new(operand),
        };

        // `DIV` binds tighter than a comparison, so it swallowed the whole
        // construct and the walk is what puts it back outside.
        let mut swallowed = like.clone();
        assert!(take_leftmost(
            &mut swallowed,
            &wrap,
            precedence(&BinaryOperator::MyIntegerDivide)
        ));
        assert_ne!(swallowed, like);

        // `AND` binds looser and holds the construct as an operand already.
        for op in [BinaryOperator::And, BinaryOperator::Xor, BinaryOperator::Or] {
            let mut operand = like.clone();
            assert!(
                !take_leftmost(&mut operand, &wrap, precedence(&op)),
                "{op} walked into a construct that binds tighter than it"
            );
            assert_eq!(operand, like);
        }

        // A comparison binds looser than a `PREDICATE` and holds one the same
        // way.
        let mut operand = like.clone();
        assert!(!take_leftmost(&mut operand, &wrap, COMPARISON));
        assert_eq!(operand, like);

        // A tie *is* walked into, because everything here is left-associative
        // and so one of equal precedence belongs outside the construct that
        // gave up its operand — the same rule the `BinaryOp` branch follows.
        // This is the level `reassociated_postfix` walks a trailing operand
        // at, and `3 LIKE '2' = 3 IN (1, 1)` is where two `PREDICATE`s meet:
        // the `IN` arrives inside the `LIKE` pattern and belongs around it.
        for (level, node) in [
            (PREDICATE, parse("a IN (1)")),
            (COMPARISON, parse("a IS NULL")),
        ] {
            let mut operand = node.clone();
            assert!(
                take_leftmost(&mut operand, &wrap, level),
                "a construct at level {level} was not walked into from its own level"
            );
            assert_ne!(operand, node);
        }
    }

    /// A prefix is corrected only when it swallowed something that binds
    /// looser than it does — for `NOT` that is `XOR` and nothing else, and for
    /// `~` it is everything, because `~` binds to the operand beside it.
    ///
    /// The allocation count is the point of the first list: both of these run
    /// on every such node of every row, and cloning the operand to find out
    /// there was nothing to do is the waste `grouped` is asked about first.
    #[test]
    fn a_prefix_is_corrected_only_when_it_swallowed_a_looser_chain() {
        for sql in [
            "NOT a",
            "NOT a = 1",
            "NOT 1 + 2",
            "NOT 0 | 1",
            "NOT a IS NULL",
            "NOT a IN (1, 2)",
            "NOT (0 XOR 1)",
            // `~` binds to its operand and nothing else, so one already
            // holding just that has nothing to give back.
            "~a",
            "~1",
            "~(1 * 2)",
        ] {
            let expr = parse(sql);
            let Expr::UnaryOp { op, expr } = &expr else {
                panic!("{sql} did not parse to a prefix");
            };
            let before = ALLOCATIONS.with(Cell::get);
            let fixed = reassociated_unary(op, expr);
            let allocated = ALLOCATIONS.with(Cell::get) - before;
            assert!(fixed.is_none(), "`{sql}` was rebuilt");
            assert_eq!(allocated, 0, "`{sql}` allocated {allocated} times");
        }

        for sql in [
            "NOT 0 XOR 1",
            "NOT 0 XOR 1 XOR 1",
            "NOT 2 || NULL IN (1)",
            "~1 * 2",
        ] {
            let expr = parse(sql);
            let Expr::UnaryOp { op, expr } = &expr else {
                panic!("{sql} did not parse to a prefix");
            };
            assert!(
                reassociated_unary(op, expr).is_some(),
                "`{sql}` was left as it arrived"
            );
        }
    }

    /// The corrected tree with its grouping written down. `Display` drops the
    /// parentheses, which is the whole of what is asserted below.
    fn shape(e: &Expr) -> String {
        match e {
            Expr::BinaryOp { left, op, right } => {
                format!("({} {op} {})", shape(left), shape(right))
            }
            Expr::IsNull(inner) => format!("({} IS NULL)", shape(inner)),
            other => other.to_string(),
        }
    }

    /// One pass is the whole correction.
    ///
    /// There are four levels below a `PREDICATE`, so sinking a construct past
    /// the root of its operand can land it on an operator that is looser than
    /// it still: `0 || 1 XOR 2 IS NULL` sinks past the `||` onto the `XOR`.
    /// Not visible in an answer — `eval` re-enters and `flatten` finishes the
    /// job — so it is asserted on the tree, which is where the claim is.
    #[test]
    fn a_postfix_construct_sinks_all_the_way_in_one_pass() {
        let expr = parse("0 || 1 XOR 2 IS NULL");
        assert_eq!(
            reassociated_postfix(&expr).as_ref().map(shape),
            Some("(0 || (1 XOR (2 IS NULL)))".to_owned())
        );
    }

    /// Both halves of the same claim for the constructs that are not
    /// operators, which `reassociated` is never called for.
    ///
    /// `a IS NULL` is the shape a soft-deleted row is found by, so the fast
    /// path matters here for the same reason it does above; and
    /// `1 XOR 2 IS NULL` is the shape that makes the correction necessary at
    /// all.
    #[test]
    fn a_postfix_test_is_corrected_only_when_it_swallowed_something() {
        for sql in [
            "a IS NULL",
            "a IS NOT NULL",
            "a + 1 IS NULL",
            "a IN (1, 2)",
            "a BETWEEN 1 AND 2",
            "a LIKE 'x%'",
            "a REGEXP 'x'",
            "(1 XOR 2) IS NULL",
        ] {
            let expr = parse(sql);
            let before = ALLOCATIONS.with(Cell::get);
            let fixed = reassociated_postfix(&expr);
            let allocated = ALLOCATIONS.with(Cell::get) - before;
            assert!(fixed.is_none(), "`{sql}` was rebuilt");
            assert_eq!(allocated, 0, "`{sql}` allocated {allocated} times");
        }

        for sql in [
            // The construct swallowed the chain: MySQL means
            // `1 XOR (2 IS NULL)`.
            "1 XOR 2 IS NULL",
            "1 XOR 2 IN (2)",
            "1 XOR 2 BETWEEN 1 AND 3",
            "1 XOR 'a' LIKE 'a'",
            // The operand after the keyword swallowed it instead.
            "'a' LIKE 'a' XOR 0",
            "'a' LIKE 'a' = 1",
            "1 BETWEEN 0 AND 2 XOR 1",
        ] {
            assert!(
                reassociated_postfix(&parse(sql)).is_some(),
                "`{sql}` was left as it arrived"
            );
        }
    }
}
