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
                UnaryOperator::Minus => Ok(match value {
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
                    return eval(result, ctx);
                }
            }
            match else_result {
                Some(e) => eval(e, ctx),
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
            Ok(rows
                .into_iter()
                .next()
                .and_then(|row| row.into_iter().next())
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
        AstValue::Boolean(b) => Value::Int(i64::from(*b)),
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

/// SQL booleans are 1 and 0.
pub fn bool_value(b: bool) -> Value {
    Value::Int(i64::from(b))
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

/// How tightly an operator binds, for the ones sqlparser binds too loosely.
///
/// Zero means sqlparser already agrees with MySQL and nothing needs moving.
/// The order among the rest is MySQL's: a path extraction is a primary, then
/// `^`, then `||` where it concatenates, then `DIV`.
fn tightness(op: &BinaryOperator) -> u8 {
    use BinaryOperator as B;
    match op {
        B::Arrow | B::LongArrow => 4,
        B::BitwiseXor => 3,
        // Two operators wearing one token: concatenating it binds tighter than
        // `DIV`, and meaning OR it binds looser than everything here.
        //
        // Read from the statement rather than from a session, which is where
        // `binary` reads it to decide what `||` *does*. Two readings of one
        // flag, so `eval` has to run inside a `crate::exec::Source` scope or
        // they can disagree and `7 DIV 2 || 3` would be shaped as OR and then
        // concatenated. Both entry points that parse set that scope.
        B::StringConcat if crate::exec::pipes_as_concat() => 2,
        B::MyIntegerDivide => 1,
        _ => 0,
    }
}

/// Re-associates an operator sqlparser binds more loosely than MySQL does.
///
/// Two of them are wrong in different ways. `DIV` is parsed by a dialect hook
/// that reads its right operand with `parse_expr`, which takes everything to
/// the end of the expression: `7 DIV 2 + 1` arrives as `7 DIV (2 + 1)` and
/// answers 2 where MySQL answers 4. `->` and `->>` are given nearly the
/// loosest precedence there is, where MySQL treats a path extraction as a
/// primary: `doc->'$.b' = 6` arrives as `doc -> ('$.b' = 6)` and asks for the
/// path `0`. Both are silent — a `WHERE` over either matches the wrong rows.
///
/// A wrapper `Dialect` that fixed this at the source is not an option here —
/// see `crate::dialect` for why — so the shape is corrected after parsing,
/// wherever the tree is read.
///
/// The operator keeps only the leftmost operand of whatever it swallowed and
/// the rest stays outside it. Anything bound tighter is a unit it cannot reach
/// into, so that unit is corrected first and then stepped through.
///
/// `None` when nothing needs moving, which is also what stops the caller
/// recursing forever.
pub(crate) fn reassociated(op: &BinaryOperator, left: &Expr, right: &Expr) -> Option<Expr> {
    let level = tightness(op);
    if level == 0 {
        return None;
    }
    let mut fixed = right.clone();
    take_leftmost(&mut fixed, left, op, level).then_some(fixed)
}

/// Puts `left <op> …` around the operand `op` should have taken, in place.
///
/// Walks down the left spine of what was swallowed. Every construct here is
/// one that extends an expression to its *left* — a binary operator, a postfix
/// test, an `IN` list — so its leftmost descendant is the operand that was
/// really adjacent. A prefix or a parenthesised group is not on that list and
/// ends the walk, which is why `7 DIV (2 + 1)` keeps its parentheses.
///
/// `false` when the node *is* the operand, so nothing needs moving.
fn take_leftmost(e: &mut Expr, left: &Expr, op: &BinaryOperator, level: u8) -> bool {
    use Expr as E;

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
        if tightness(inner) > level {
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
    if !take_leftmost(child, left, op, level) {
        let operand = child.clone();
        *child = E::BinaryOp {
            left: Box::new(left.clone()),
            op: op.clone(),
            right: Box::new(operand),
        };
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
    let stamp = |e: &Expr| -> EngineResult<Option<temporal::Stamp>> {
        let value = eval(e, ctx)?;
        if value.is_null() {
            return Ok(None);
        }
        Ok(temporal::parse(&value.as_text()))
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
                let Some(to) = stamp(second)? else {
                    return Ok(Value::Null);
                };
                let Some(amount) = eval(first, ctx)?.as_int() else {
                    return Ok(Value::Null);
                };
                return Ok(match to.add(unit, amount) {
                    Some(moved) => Value::text(moved.render()),
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
            let Some(stamp) = stamp(subject)? else {
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
            Ok(match stamp.add(unit, signed) {
                Some(moved) => Value::text(moved.render()),
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
