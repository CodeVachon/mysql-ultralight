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
                UnaryOperator::Minus => Ok(match value {
                    Value::Null => Value::Null,
                    Value::Int(v) => Value::Int(-v),
                    other => other
                        .as_float()
                        .map(|f| Value::Float(-f))
                        .unwrap_or(Value::Null),
                }),
                UnaryOperator::Plus => Ok(value),
                other => Err(unsupported(format!("unary operator `{other}`"))),
            }
        }

        Expr::BinaryOp { left, op, right } => binary(op, eval(left, ctx)?, eval(right, ctx)?, ctx),

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
            functions::cast(value, data_type)
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

        B::Plus | B::Minus | B::Multiply | B::Divide | B::Modulo => arithmetic(op, &left, &right)?,

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

fn arithmetic(op: &BinaryOperator, left: &Value, right: &Value) -> EngineResult<Value> {
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
            _ => unreachable!("caller restricts the operator set"),
        });
    }

    let (Some(a), Some(b)) = (left.as_float(), right.as_float()) else {
        return Ok(Value::Null);
    };

    Ok(match op {
        B::Plus => Value::Float(a + b),
        B::Minus => Value::Float(a - b),
        B::Multiply => Value::Float(a * b),
        B::Divide if b == 0.0 => Value::Null,
        B::Divide => Value::Float(a / b),
        B::Modulo if b == 0.0 => Value::Null,
        B::Modulo => Value::Float(a % b),
        _ => unreachable!("caller restricts the operator set"),
    })
}
