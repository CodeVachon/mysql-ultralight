//! Expression evaluation.

use sqlparser::ast::{
    BinaryOperator, CaseWhen, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Query,
    UnaryOperator, Value as AstValue,
};

use crate::error::{unsupported, EngineError, EngineResult};
use crate::functions;
use crate::session::Session;
use crate::value::Value;

/// One column visible to an expression.
#[derive(Debug, Clone)]
pub struct ColumnRef {
    /// Table name or alias the column is reachable through, empty when none.
    pub table: String,
    pub name: String,
}

impl ColumnRef {
    pub fn new(table: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            name: name.into(),
        }
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
    fn lookup(&self, table: Option<&str>, name: &str) -> EngineResult<Value> {
        let Some(row) = &self.row else {
            return Err(EngineError::unknown_column(name, "field list"));
        };

        let mut found: Option<&Value> = None;
        for (i, column) in row.schema.iter().enumerate() {
            if !column.name.eq_ignore_ascii_case(name) {
                continue;
            }
            if let Some(t) = table {
                if !column.table.eq_ignore_ascii_case(t) {
                    continue;
                }
            }
            // An unqualified name matching two tables is ambiguous in SQL, but
            // Prisma always qualifies, so the first match is taken rather than
            // erroring on a case that does not arise.
            found = row.values.get(i);
            break;
        }

        found
            .cloned()
            .ok_or_else(|| EngineError::unknown_column(name, "field list"))
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
pub fn probing<T>(f: impl FnOnce() -> T) -> T {
    PROBING.with(|p| p.set(true));
    let out = f();
    PROBING.with(|p| p.set(false));
    out
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
    // Bound before the match, because two arms below shadow `expr` with the
    // operand they destructure and still need the node itself.
    let expr_node = expr;
    match expr {
        Expr::Identifier(ident) if ident.value.starts_with('@') => variable(&ident.value, ctx),

        Expr::Identifier(ident) => ctx.lookup(None, &ident.value),

        // `@@GLOBAL.version` parses as a compound identifier.
        Expr::CompoundIdentifier(parts)
            if parts.first().is_some_and(|p| p.value.starts_with('@')) =>
        {
            let name: Vec<&str> = parts.iter().map(|p| p.value.as_str()).collect();
            variable(&name.join("."), ctx)
        }

        Expr::CompoundIdentifier(parts) => {
            let name = parts.last().map(|i| i.value.as_str()).unwrap_or_default();
            // `db.table.column` and `table.column` both qualify by the part
            // immediately before the column.
            let table = parts
                .get(parts.len().wrapping_sub(2))
                .map(|i| i.value.as_str());
            // No fallback to an unqualified lookup: an out-of-scope qualifier
            // would otherwise bind to another table's same-named column and
            // silently drive a write against the wrong rows.
            ctx.lookup(table, name).map_err(|_| {
                let shown = parts
                    .iter()
                    .map(|p| p.value.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                EngineError::unknown_column(&shown, "field list")
            })
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
                UnaryOperator::Minus => Ok(match value.untag_bool() {
                    Value::Null => Value::Null,
                    Value::Int(v) => Value::Int(-v),
                    other => Value::Float(-other.as_number()),
                }),
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
            match (v.compare(&lo), v.compare(&hi)) {
                (Some(a), Some(b)) => {
                    let inside = a != std::cmp::Ordering::Less && b != std::cmp::Ordering::Greater;
                    Ok(bool_value(inside != *negated))
                }
                _ => Ok(Value::Null),
            }
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
            let value = eval(expr, ctx)?;
            let pattern = eval(pattern, ctx)?;
            if value.is_null() || pattern.is_null() {
                return Ok(Value::Null);
            }
            // MySQL escapes with a backslash unless told otherwise. ORMs
            // escape user input that way and omit the ESCAPE clause, so
            // leaving this as None makes `contains` filters return nothing.
            let escape = escape_char
                .as_ref()
                .and_then(|s| s.to_string().chars().next())
                .or(Some('\\'));
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
            for CaseWhen { condition, result } in conditions {
                let hit = match &subject {
                    // `CASE x WHEN y` compares; `CASE WHEN c` tests truthiness.
                    Some(s) => s.compare(&eval(condition, ctx)?) == Some(std::cmp::Ordering::Equal),
                    None => eval(condition, ctx)?.truthy() == Some(true),
                };
                if hit {
                    // Untagged, because `CASE` builds a result type out of
                    // its branches and a boolean is not one of the types it
                    // can build: `JSON_ARRAY(CASE WHEN 1 THEN TRUE ELSE FALSE
                    // END)` is `[1]` on 8.3. An ENUM survives the same
                    // construct, so the two tags part company here.
                    return Ok(eval(result, ctx)?.untag_bool());
                }
            }
            match else_result {
                Some(e) => Ok(eval(e, ctx)?.untag_bool()),
                None => Ok(Value::Null),
            }
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
            let rows = ctx.run(subquery)?;
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

        // `x COLLATE c` compares x under a different collation. Comparison
        // here is case-insensitive, so a `_ci` collation is honoured by doing
        // nothing; `_bin` and `_cs` would compare differently and are refused
        // rather than ignored.
        //
        // One gap is knowingly left open: comparison here is accent-*sensitive*,
        // so the accent-insensitive collations — `utf8mb4_unicode_ci`,
        // `utf8mb4_0900_ai_ci` — fold `café` and `cafe` together in MySQL and
        // not here. That is a property of every comparison the engine makes,
        // including the server's default collation, rather than something this
        // expression introduces, so refusing it here would reject the schema
        // while leaving the same difference everywhere else.
        Expr::Collate { expr, collation } => {
            let name = collation.to_string().to_ascii_lowercase();
            let name = name.trim_matches('`');
            if !name.ends_with("_ci") {
                return Err(unsupported(format!("the `{name}` collation")));
            }
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

/// Converts a literal from the AST.
pub fn literal(value: &AstValue) -> EngineResult<Value> {
    Ok(match value {
        AstValue::Number(n, _) => match n.parse::<i64>() {
            Ok(v) => Value::Int(v),
            Err(_) => Value::Float(n.parse::<f64>().unwrap_or(0.0)),
        },
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

        B::Plus | B::Minus | B::Multiply | B::Divide | B::Modulo | B::MyIntegerDivide => {
            arithmetic(op, &left, &right)?
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
fn precedence(op: &BinaryOperator) -> u8 {
    use BinaryOperator as B;
    match op {
        B::Arrow | B::LongArrow => 9,
        // Two operators wearing one token: concatenating it binds tighter than
        // everything but a path extraction, and meaning OR it binds looser
        // than everything here.
        //
        // Read from the statement rather than from a session, which is where
        // `binary` reads it to decide what `||` *does*. Two readings of one
        // flag, so `eval` has to run inside a `crate::exec::Source` scope or
        // they can disagree and `7 DIV 2 || 3` would be shaped as OR and then
        // concatenated. Both entry points that parse set that scope.
        B::StringConcat if crate::exec::pipes_as_concat() => 8,
        B::BitwiseXor => 7,
        B::Multiply | B::Divide | B::Modulo | B::MyIntegerDivide => 6,
        B::Plus | B::Minus => 5,
        B::PGBitwiseShiftLeft | B::PGBitwiseShiftRight => 4,
        B::BitwiseAnd => 3,
        B::BitwiseOr => 2,
        _ => 0,
    }
}

/// Tighter than any binary operator, which is where MySQL puts a prefix.
///
/// `~1*2` is `(~1)*2` there — it overflows and is refused — where `~(1*2)` is
/// 18446744073709551613 and is not.
const PREFIX: u8 = 10;

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
        // Constructs that extend an expression to their *left*, which is what
        // `take_leftmost` walks: `7 DIV 2 IS NULL` arrives as
        // `7 DIV (2 IS NULL)` and only that walk puts the `IS NULL` back
        // outside. The list is that walk's, and has to stay it.
        E::IsNull(_)
        | E::IsNotNull(_)
        | E::IsTrue(_)
        | E::IsFalse(_)
        | E::IsNotTrue(_)
        | E::IsNotFalse(_)
        | E::IsUnknown(_)
        | E::IsNotUnknown(_)
        | E::InList { .. }
        | E::InSubquery { .. }
        | E::InUnnest { .. }
        | E::Between { .. }
        | E::Like { .. }
        | E::ILike { .. }
        | E::SimilarTo { .. }
        | E::RLike { .. }
        | E::IsDistinctFrom(..)
        | E::IsNotDistinctFrom(..) => None,
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
        _ => e,
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
    if !matches!(op, UnaryOperator::BitwiseNot) {
        return None;
    }
    let mut fixed = expr.clone();
    let wrap = |operand: Expr| Expr::UnaryOp {
        op: *op,
        expr: Box::new(operand),
    };
    take_leftmost(&mut fixed, &wrap, PREFIX).then_some(fixed)
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
        E::IsNull(x)
        | E::IsNotNull(x)
        | E::IsTrue(x)
        | E::IsFalse(x)
        | E::IsNotTrue(x)
        | E::IsNotFalse(x)
        | E::IsUnknown(x)
        | E::IsNotUnknown(x) => x,
        E::InList { expr, .. } | E::InSubquery { expr, .. } | E::InUnnest { expr, .. } => expr,
        E::Between { expr, .. } => expr,
        E::Like { expr, .. }
        | E::ILike { expr, .. }
        | E::SimilarTo { expr, .. }
        | E::RLike { expr, .. } => expr,
        E::IsDistinctFrom(x, _) | E::IsNotDistinctFrom(x, _) => x,
        _ => return false,
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

/// The value an arithmetic operator produces.
///
/// `pub(crate)` because `MOD` is the `%` operator spelled as a call and has to
/// answer the same thing: typed alike and evaluated apart, `MOD(1.500, 2)`
/// promised a decimal and handed back the integer 1.
pub(crate) fn arithmetic(op: &BinaryOperator, left: &Value, right: &Value) -> EngineResult<Value> {
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
    if let (Value::Int(a), Value::Int(b)) = (left, right) {
        return Ok(match op {
            B::Plus => Value::Int(a.wrapping_add(*b)),
            B::Minus => Value::Int(a.wrapping_sub(*b)),
            B::Multiply => Value::Int(a.wrapping_mul(*b)),
            // Division by zero yields NULL in MySQL, not an error.
            B::Divide => {
                if *b == 0 {
                    Value::Null
                } else {
                    Value::Float(*a as f64 / *b as f64)
                }
            }
            B::Modulo => {
                if *b == 0 {
                    Value::Null
                } else {
                    Value::Int(a.wrapping_rem(*b))
                }
            }
            // `DIV` truncates toward zero rather than flooring: MySQL answers
            // -3 for `-7 DIV 2`, where a floor would say -4. Rust's integer
            // division truncates the same way.
            B::MyIntegerDivide => {
                if *b == 0 {
                    Value::Null
                } else {
                    // The one quotient that does not fit, and MySQL refuses it
                    // rather than wrapping — unlike the additions above, which
                    // it lets wrap.
                    Value::Int(a.checked_div(*b).ok_or_else(bigint_out_of_range)?)
                }
            }
            _ => unreachable!("caller restricts the operator set"),
        });
    }

    // Exact where both sides are exact, which a fractional temporal makes
    // reachable: a DATETIME(3) packs to a decimal, and `dt3 + 0` through an
    // `f64` renders 20240105060708.5 where MySQL says 20240105060708.500.
    if let Some(exact) = exact_arithmetic(op, left, right) {
        return Ok(exact);
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
/// Only `+`, `-` and `*`, and only where a `Decimal` is involved — two `Int`s
/// are answered above. `/` is left out deliberately: MySQL widens a quotient
/// by `div_precision_increment` and this engine does not, so `1 / 3` is
/// already 0.3333333333333333 here against MySQL's 0.3333, and adding half of
/// that rule to one operand kind would make the gap depend on what was
/// divided.
///
/// The scales are MySQL's: a sum or a difference keeps the wider of the two,
/// and a product their total, so `CAST('2.5' AS DECIMAL(10,1)) * 2` is 5.0
/// rather than 5. `None` on any overflow, which falls back to the `f64` path
/// rather than refusing a statement MySQL answers.
fn exact_arithmetic(op: &BinaryOperator, left: &Value, right: &Value) -> Option<Value> {
    use BinaryOperator as B;
    if !matches!(op, B::Plus | B::Minus | B::Multiply) {
        return None;
    }
    let scale = |v: &Value| match v {
        Value::Int(_) => Some(0usize),
        Value::Decimal(text) => Some(crate::decimal::scale_of(text)),
        _ => None,
    };
    let (left_scale, right_scale) = (scale(left)?, scale(right)?);
    let units = |v: &Value, at: usize| match v {
        // Checked, because the scale comes from a value's own text and nothing
        // bounds how many digits that has.
        Value::Int(n) => i128::from(*n).checked_mul(10i128.checked_pow(u32::try_from(at).ok()?)?),
        Value::Decimal(text) => crate::decimal::units(text, at),
        _ => None,
    };
    let (total, scale) = match op {
        B::Multiply => (
            units(left, left_scale)?.checked_mul(units(right, right_scale)?)?,
            left_scale.checked_add(right_scale)?,
        ),
        _ => {
            let widest = left_scale.max(right_scale);
            let (a, b) = (units(left, widest)?, units(right, widest)?);
            let total = if matches!(op, B::Plus) {
                a.checked_add(b)?
            } else {
                a.checked_sub(b)?
            };
            (total, widest)
        }
    };
    Some(Value::Decimal(crate::decimal::render(total, scale)))
}

/// A bitwise chain's answer as a `Value`, or a refusal.
///
/// MySQL answers every bitwise operator as a `BIGINT UNSIGNED` and this engine
/// holds an `i64`, so an answer above `i64::MAX` has no value to be. Keeping
/// the bits and letting the column's UNSIGNED flag reinterpret them looks like
/// it works — `SELECT ~0` prints 18446744073709551615 that way — but only for
/// a value that leaves as a column of its own and is never read again. *Every*
/// other reader sees the raw `i64`, and nothing on the value tells them not
/// to: measured against 8.3, `~0 / 2` answered -0.5 for 9223372036854775807.5,
/// `~0 MOD 10` answered 18446744073709551615 for 5, `CONCAT(~0)` answered -1,
/// `~0 > 5` answered 0 for 1, and a `UNION` dropped the flag and printed -1.
/// Those are wrong answers rather than gaps, which this engine does not trade
/// for coverage.
///
/// So the answer is refused where it cannot be represented, and only there:
/// the check is at the chain's boundary, so everything the chain can compute
/// exactly it still computes. What refuses is a bitwise expression whose own
/// value is above `i64::MAX` — `SELECT ~0` and `SELECT 1 << 63` — and nothing
/// else. `SELECT ~0 & 255` is 255 and `SELECT 1 + 3 & ~5` is 0.
///
/// Representing the rest means giving `Value` an unsigned variant, which
/// reaches the state image, every comparison, and every function's `match` —
/// its own issue rather than a corner of this one.
fn bit_pattern(bits: u64) -> EngineResult<Value> {
    i64::try_from(bits).map(Value::Int).map_err(|_| {
        unsupported(format!(
            "the bitwise result {bits}, which is above the largest signed 64-bit integer"
        ))
    })
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
/// that an intermediate wider than `i64` never has to become a `Value`. Every
/// operator here is exact on the bits whatever their sign, which is why the
/// chain composes and the boundary is the only place a range matters.
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
    // The token before the value, because the token carries a type the value
    // does not. A literal is never NULL, so this runs ahead of the NULL check
    // rather than after it.
    if let Some(bits) = literal_bits(e) {
        return Ok(Some(bits));
    }
    let value = eval(e, ctx)?;
    if value.is_null() {
        return Ok(None);
    }
    bit_operand(&value).map(Some)
}

/// The bits an operand written as a literal contributes.
///
/// A literal is the one operand whose type is in the statement rather than on
/// the value, and reading it off the token is what keeps the refusal in
/// [`bit_operand`] narrow: a quoted string *is* a string however much its
/// characters look like a date, so `'2026-01-02' | 0` still answers 2026 —
/// MySQL's answer, and the one the DATE column beside it does not share.
///
/// A number with a point is the same argument the other way. MySQL types it
/// DECIMAL and evaluates it exactly where the engine evaluates it to an `f64`,
/// which loses the tie at the seventeenth digit: `4503599627370496.5 | 0` is
/// 4503599627370497 there and 4503599627370496 through a double. An exponent
/// is a DOUBLE to MySQL too, so `1e2 | 0` is left to the value.
fn literal_bits(e: &Expr) -> Option<u64> {
    let (negative, number) = number_token(e)?;
    // A point and no exponent is what MySQL types DECIMAL; `1e2` is a DOUBLE
    // there as much as here, so it keeps the reading it had. Tested on the
    // borrowed token, so an operand that is not one of these — every column
    // reference, every row of `WHERE id & 1` — costs no allocation at all.
    if !number.contains('.') || number.contains(['e', 'E']) {
        return None;
    }
    Some(if negative {
        decimal_bits(&format!("-{number}"))
    } else {
        decimal_bits(number)
    })
}

/// The numeric token a literal operand spells, and whether it is negated.
///
/// A sign is a unary operator to the parser and part of the number to MySQL,
/// so the signs are folded here. Counted rather than concatenated: building
/// `"-{token}"` at each step spelled `- -2.5` as `"--2.5"`, which is not a
/// decimal, so it fell to the leading-number reader and answered 0 where MySQL
/// and this engine's own `- -2.5` both answer 3.
///
/// Parentheses are transparent, because they group a number without retyping
/// it: `-(4503599627370496.5)` is the same DECIMAL as its unparenthesised
/// twin and has to read the same way.
fn number_token(e: &Expr) -> Option<(bool, &str)> {
    match e {
        Expr::Nested(inner) => number_token(inner),
        Expr::Value(v) => match &v.value {
            AstValue::Number(n, _) => Some((false, n)),
            _ => None,
        },
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => number_token(expr),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => {
            let (negative, number) = number_token(expr)?;
            Some((!negative, number))
        }
        _ => None,
    }
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
}
