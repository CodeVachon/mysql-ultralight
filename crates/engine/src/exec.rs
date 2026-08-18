//! Statement execution.

use std::sync::{Arc, RwLock};

use mysql_ultralight_protocol::ColumnType;
use sqlparser::ast::{
    Assignment, AssignmentTarget, Expr, GroupByExpr, Insert, JoinOperator, ObjectName, ObjectType,
    OrderBy, OrderByKind, Query, Select, SelectItem, SetExpr, Statement, TableFactor,
    TableWithJoins,
};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use crate::catalog::{Catalog, Table, INFORMATION_SCHEMA};
use crate::ddl;
use crate::error::{unsupported, EngineError, EngineResult};
use crate::expr::{self, ColumnRef, Context, Row};
use crate::info_schema;
use crate::params;
use crate::relation::Relation;
use crate::result::{OutColumn, Outcome, ResultSet};
use crate::session::{ServerConfig, Session};
use crate::value::Value;

/// The database server's shared state.
#[derive(Debug)]
pub struct Engine {
    catalog: RwLock<Catalog>,
    config: ServerConfig,
}

impl Engine {
    pub fn new(config: ServerConfig) -> Arc<Self> {
        Arc::new(Self {
            catalog: RwLock::new(Catalog::new()),
            config,
        })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Creates a database at startup, so a server can be handed one ready.
    pub fn bootstrap_database(&self, name: &str) -> EngineResult<()> {
        self.catalog_mut()?.create_database(name, true)
    }

    fn catalog(&self) -> EngineResult<std::sync::RwLockReadGuard<'_, Catalog>> {
        self.catalog.read().map_err(|_| poisoned())
    }

    fn catalog_mut(&self) -> EngineResult<std::sync::RwLockWriteGuard<'_, Catalog>> {
        self.catalog.write().map_err(|_| poisoned())
    }

    /// Write access for the DDL module, which lives outside this impl.
    pub(crate) fn catalog_mut_public(
        &self,
    ) -> EngineResult<std::sync::RwLockWriteGuard<'_, Catalog>> {
        self.catalog_mut()
    }

    pub(crate) fn split_name_public(
        &self,
        session: &Session,
        name: &ObjectName,
    ) -> EngineResult<(String, String)> {
        self.split_name(session, name)
    }

    /// Parses and executes one statement.
    ///
    /// Parameters are substituted into the text before parsing, so a
    /// placeholder can never reach evaluation unbound.
    pub fn execute(
        &self,
        session: &mut Session,
        sql: &str,
        parameters: &[Value],
    ) -> EngineResult<Outcome> {
        let bound = params::bind(sql, parameters);
        let dialect = MySqlDialect {};

        let statements =
            Parser::parse_sql(&dialect, &bound).map_err(|e| EngineError::parse(format!("{e}")))?;

        match statements.len() {
            0 => Ok(Outcome::ok()),
            1 => self.run(session, &statements[0]),
            // Multi-statement requires MORE_RESULTS_EXISTS on the wire, which
            // the server does not advertise. Erroring beats returning only the
            // first result and letting the caller believe it ran everything.
            _ => Err(unsupported("multiple statements in one command")),
        }
    }

    fn run(&self, session: &mut Session, statement: &Statement) -> EngineResult<Outcome> {
        match statement {
            Statement::Query(query) => self.query(session, query).map(Outcome::Rows),
            Statement::Insert(insert) => self.insert(session, insert),
            Statement::Update(update) => self.update(
                session,
                &update.table,
                &update.assignments,
                update.selection.as_ref(),
            ),
            Statement::Delete(delete) => self.delete(session, delete),

            Statement::CreateTable(create) => ddl::create_table(self, session, create),
            Statement::AlterTable(alter) => ddl::alter_table(
                self,
                session,
                &alter.name,
                &alter.operations,
                alter.if_exists,
            ),
            Statement::CreateIndex(create) => ddl::create_index(self, session, create),
            Statement::CreateDatabase {
                db_name,
                if_not_exists,
                ..
            } => {
                self.catalog_mut()?
                    .create_database(&object_name(db_name), *if_not_exists)?;
                Ok(Outcome::ok())
            }
            Statement::Drop {
                object_type,
                names,
                if_exists,
                ..
            } => self.drop(session, *object_type, names, *if_exists),
            Statement::Truncate(truncate) => {
                for name in &truncate.table_names {
                    let (db, table) = self.split_name(session, &name.name)?;
                    let mut catalog = self.catalog_mut()?;
                    let database = catalog
                        .database_mut(&db)
                        .ok_or_else(|| EngineError::unknown_database(&db))?;
                    database
                        .table_mut(&table)
                        .ok_or_else(|| EngineError::no_such_table(&table))?
                        .rows
                        .clear();
                }
                Ok(Outcome::ok())
            }

            Statement::Use(use_stmt) => {
                let name = format!("{use_stmt}")
                    .trim_start_matches("USE ")
                    .trim_matches('`')
                    .to_owned();
                // information_schema is served virtually rather than stored,
                // so it is absent from the catalog while still being listed by
                // SHOW DATABASES and queryable when qualified.
                if !name.eq_ignore_ascii_case(INFORMATION_SCHEMA)
                    && !self.catalog()?.contains(&name)
                {
                    return Err(EngineError::unknown_database(&name));
                }
                session.current_database = Some(name);
                Ok(Outcome::ok())
            }

            Statement::Set(set) => self.set(session, set),

            Statement::ShowVariables { .. } => Ok(Outcome::Rows(self.show_variables(session))),
            Statement::ShowTables { .. } => self.show_tables(session).map(Outcome::Rows),
            Statement::ShowDatabases { .. } => self.show_databases().map(Outcome::Rows),
            Statement::ShowColumns { show_options, .. } => {
                let table = show_options
                    .show_in
                    .as_ref()
                    .and_then(|s| s.parent_name.as_ref())
                    .map(object_name)
                    .unwrap_or_default();
                self.show_columns(session, &table).map(Outcome::Rows)
            }

            // Transaction control is accepted so client code paths work, but
            // there is no rollback yet: a test database that discards state at
            // process exit has nothing to recover.
            Statement::StartTransaction { .. } => {
                session.in_transaction = true;
                Ok(Outcome::ok())
            }
            Statement::Commit { .. } | Statement::Rollback { .. } => {
                session.in_transaction = false;
                Ok(Outcome::ok())
            }
            Statement::Savepoint { .. } | Statement::ReleaseSavepoint { .. } => Ok(Outcome::ok()),

            other => Err(unsupported(format!(
                "statement `{}`",
                first_words(&other.to_string(), 3)
            ))),
        }
    }

    // ---------------------------------------------------------------- SELECT

    fn query(&self, session: &Session, query: &Query) -> EngineResult<ResultSet> {
        let SetExpr::Select(select) = query.body.as_ref() else {
            return Err(unsupported("this query form"));
        };

        let result = self.select(session, select, query.order_by.as_ref())?;
        // Ordering by an output alias rather than a source column is legal and
        // could not be resolved against the table, so it is retried here.
        let pending = result.pending_order;
        let mut rows = result.result;
        if pending {
            self.order_projected(&mut rows, session, query.order_by.as_ref())?;
        }
        apply_limit(&mut rows, session, query)?;
        Ok(rows)
    }

    fn select(
        &self,
        session: &Session,
        select: &Select,
        order: Option<&OrderBy>,
    ) -> EngineResult<Ordered> {
        // A SELECT with no FROM evaluates its projection once, which is the
        // shape of `SELECT 1` and every driver's preamble query.
        if select.from.is_empty() {
            return Ok(Ordered::sorted(self.select_without_table(session, select)?));
        }
        if select.from.len() > 1 {
            return Err(unsupported("comma-separated FROM items"));
        }

        let relation = self.build_relation(session, &select.from[0])?;
        let refs = relation.refs();

        // Filter first so projections and aggregates see only matching rows.
        let mut matching: Vec<&Vec<Value>> = Vec::new();
        for row in &relation.rows {
            let ctx = Context {
                row: Some(Row {
                    schema: &refs,
                    values: row,
                }),
                session,
            };
            let keep = match &select.selection {
                Some(predicate) => expr::eval(predicate, &ctx)?.truthy() == Some(true),
                None => true,
            };
            if keep {
                matching.push(row);
            }
        }

        let grouped = match &select.group_by {
            GroupByExpr::Expressions(exprs, _) => !exprs.is_empty(),
            GroupByExpr::All(_) => true,
        };
        if grouped {
            return Err(unsupported("GROUP BY"));
        }

        if projection_is_aggregate(&select.projection) {
            // An aggregate collapses to one row, so ordering is meaningless.
            return Ok(Ordered::sorted(
                self.aggregate(session, select, &refs, &matching)?,
            ));
        }

        // Sorting happens before projection so ORDER BY can reference a column
        // the SELECT list does not return, which MySQL allows.
        let pending_order = match order {
            Some(order) => !self.sort_rows(session, &refs, &mut matching, order)?,
            None => false,
        };

        let mut projected = self.project(session, select, &relation, &refs, &matching)?;
        apply_distinct(&mut projected, select.distinct.as_ref())?;
        Ok(Ordered {
            result: projected,
            pending_order,
        })
    }

    /// Sorts source rows in place. Returns false when an ORDER BY expression
    /// cannot be resolved against the relation, leaving it for the caller to
    /// retry against the projected output.
    fn sort_rows(
        &self,
        session: &Session,
        refs: &[ColumnRef],
        rows: &mut [&Vec<Value>],
        order: &OrderBy,
    ) -> EngineResult<bool> {
        let OrderByKind::Expressions(items) = &order.kind else {
            return Err(unsupported("this ORDER BY form"));
        };

        let mut keys: Vec<Vec<Value>> = Vec::with_capacity(rows.len());
        for row in rows.iter() {
            let ctx = Context {
                row: Some(Row {
                    schema: refs,
                    values: row,
                }),
                session,
            };
            let mut key = Vec::with_capacity(items.len());
            for item in items {
                match expr::eval(&item.expr, &ctx) {
                    Ok(v) => key.push(v),
                    Err(_) => return Ok(false),
                }
            }
            keys.push(key);
        }

        let descending: Vec<bool> = items.iter().map(|i| i.options.asc == Some(false)).collect();
        let mut paired: Vec<(Vec<Value>, &Vec<Value>)> =
            keys.into_iter().zip(rows.iter().copied()).collect();
        paired.sort_by(|a, b| compare_keys(&a.0, &b.0, &descending));
        for (slot, (_, row)) in rows.iter_mut().zip(paired) {
            *slot = row;
        }
        Ok(true)
    }

    /// Materialises a FROM clause, joining as needed.
    fn build_relation(&self, session: &Session, from: &TableWithJoins) -> EngineResult<Relation> {
        let mut left = self.relation_of(session, &from.relation)?;

        for join in &from.joins {
            let right = self.relation_of(session, &join.relation)?;
            left = self.join(session, left, right, &join.join_operator)?;
        }

        Ok(left)
    }

    /// Nested-loop join of two materialised relations.
    fn join(
        &self,
        session: &Session,
        left: Relation,
        right: Relation,
        operator: &JoinOperator,
    ) -> EngineResult<Relation> {
        use sqlparser::ast::JoinConstraint;

        let (constraint, outer_left, outer_right) = match operator {
            JoinOperator::Inner(c) | JoinOperator::Join(c) => (Some(c), false, false),
            JoinOperator::LeftOuter(c) | JoinOperator::Left(c) => (Some(c), true, false),
            JoinOperator::RightOuter(c) | JoinOperator::Right(c) => (Some(c), false, true),
            JoinOperator::CrossJoin(_) => (None, false, false),
            other => return Err(unsupported(format!("join type `{other:?}`"))),
        };

        let columns = left.joined_columns(&right, outer_left || outer_right);
        let refs: Vec<ColumnRef> = columns
            .iter()
            .map(|c| ColumnRef::new(&c.table, &c.name))
            .collect();

        let condition = match constraint {
            Some(JoinConstraint::On(e)) => Some(e.clone()),
            Some(JoinConstraint::None) | None => None,
            // USING(a, b) is equality on each named column.
            Some(JoinConstraint::Using(names)) => Some(using_condition(names, &left, &right)?),
            Some(other) => return Err(unsupported(format!("join constraint `{other:?}`"))),
        };

        let null_right = vec![Value::Null; right.columns.len()];
        let null_left = vec![Value::Null; left.columns.len()];
        let mut rows = Vec::new();
        let mut right_matched = vec![false; right.rows.len()];

        for left_row in &left.rows {
            let mut matched = false;
            for (i, right_row) in right.rows.iter().enumerate() {
                let mut combined = left_row.clone();
                combined.extend_from_slice(right_row);

                let keep = match &condition {
                    Some(e) => {
                        let ctx = Context {
                            row: Some(Row {
                                schema: &refs,
                                values: &combined,
                            }),
                            session,
                        };
                        expr::eval(e, &ctx)?.truthy() == Some(true)
                    }
                    None => true,
                };

                if keep {
                    matched = true;
                    right_matched[i] = true;
                    rows.push(combined);
                }
            }
            // A LEFT JOIN keeps the left row even with no match, padded right.
            if outer_left && !matched {
                let mut combined = left_row.clone();
                combined.extend_from_slice(&null_right);
                rows.push(combined);
            }
        }

        if outer_right {
            for (i, right_row) in right.rows.iter().enumerate() {
                if !right_matched[i] {
                    let mut combined = null_left.clone();
                    combined.extend_from_slice(right_row);
                    rows.push(combined);
                }
            }
        }

        Ok(Relation { columns, rows })
    }

    /// Materialises one table factor.
    fn relation_of(&self, session: &Session, factor: &TableFactor) -> EngineResult<Relation> {
        match factor {
            TableFactor::Table { name, alias, .. } => {
                let (db, table_name) = self.split_name(session, name)?;
                let mut table = if db.eq_ignore_ascii_case(INFORMATION_SCHEMA) {
                    let catalog = self.catalog()?;
                    info_schema::table(&catalog, session, &table_name)?
                } else {
                    let catalog = self.catalog()?;
                    catalog
                        .database(&db)
                        .ok_or_else(|| EngineError::unknown_database(&db))?
                        .table(&table_name)
                        .cloned()
                        .ok_or_else(|| EngineError::no_such_table(&table_name))?
                };
                // An alias replaces the table name for qualified references.
                if let Some(alias) = alias {
                    table.name = alias.name.value.clone();
                }
                Ok(Relation::from_table(&db, &table))
            }
            TableFactor::Derived {
                subquery, alias, ..
            } => {
                let inner = self.query(session, subquery)?;
                let name = alias
                    .as_ref()
                    .map(|a| a.name.value.clone())
                    .unwrap_or_else(|| "derived".to_owned());
                Ok(Relation {
                    columns: inner
                        .columns
                        .iter()
                        .map(|c| crate::relation::RelColumn {
                            schema: String::new(),
                            table: name.clone(),
                            name: c.name.clone(),
                            ty: crate::types::SqlType {
                                kind: crate::types::TypeKind::Text,
                                declared: "varchar(512)".to_owned(),
                                length: None,
                                scale: None,
                                unsigned: false,
                                variants: Vec::new(),
                            },
                            nullable: true,
                        })
                        .collect(),
                    rows: inner.rows,
                })
            }
            other => Err(unsupported(format!("FROM item `{other}`"))),
        }
    }

    fn select_without_table(&self, session: &Session, select: &Select) -> EngineResult<ResultSet> {
        let ctx = Context::rowless(session);
        let mut columns = Vec::new();
        let mut row = Vec::new();

        for item in &select.projection {
            let (alias, e) = match item {
                SelectItem::UnnamedExpr(e) => (render_expr(e), e),
                SelectItem::ExprWithAlias { expr, alias } => (alias.value.clone(), expr),
                other => return Err(unsupported(format!("select item `{other}`"))),
            };
            let value = self.eval_scalar(session, e, &ctx)?;
            columns.push(OutColumn::new(alias, wire_type(&value)));
            row.push(value);
        }

        Ok(ResultSet {
            columns,
            rows: vec![row],
        })
    }

    /// Evaluates an expression that may reference a system or user variable.
    ///
    /// sqlparser models `@@x` and `@x` as identifiers with the sigil retained,
    /// so they are resolved here rather than inside the evaluator, which has
    /// no session-variable concept.
    fn eval_scalar(&self, session: &Session, e: &Expr, ctx: &Context<'_>) -> EngineResult<Value> {
        if let Some(value) = self.session_variable(session, e)? {
            return Ok(value);
        }
        expr::eval(e, ctx)
    }

    fn session_variable(&self, session: &Session, e: &Expr) -> EngineResult<Option<Value>> {
        let name = match e {
            Expr::Identifier(ident) => ident.value.clone(),
            // `@@GLOBAL.version` parses as a compound identifier.
            Expr::CompoundIdentifier(parts)
                if parts.first().is_some_and(|p| p.value.starts_with('@')) =>
            {
                parts
                    .iter()
                    .map(|p| p.value.as_str())
                    .collect::<Vec<_>>()
                    .join(".")
            }
            _ => return Ok(None),
        };

        if let Some(rest) = name.strip_prefix("@@") {
            return match session.system_variable(rest) {
                Some(v) => Ok(Some(v)),
                // An unknown system variable is an error in MySQL too, and
                // guessing would hide a real incompatibility.
                None => Err(EngineError::new(
                    1193,
                    "HY000",
                    format!("Unknown system variable '{rest}'"),
                )),
            };
        }
        if let Some(rest) = name.strip_prefix('@') {
            return Ok(Some(session.user_variable(rest)));
        }
        Ok(None)
    }

    fn project(
        &self,
        session: &Session,
        select: &Select,
        relation: &Relation,
        refs: &[ColumnRef],
        matching: &[&Vec<Value>],
    ) -> EngineResult<ResultSet> {
        let mut columns: Vec<OutColumn> = Vec::new();
        let mut plan: Vec<Projection> = Vec::new();

        let push_all =
            |columns: &mut Vec<OutColumn>, plan: &mut Vec<Projection>, table: Option<&str>| {
                for (i, column) in relation.columns.iter().enumerate() {
                    if table.is_some_and(|t| !column.table.eq_ignore_ascii_case(t)) {
                        continue;
                    }
                    columns.push(
                        OutColumn::from_sql_type(&column.name, &column.ty, column.nullable)
                            .in_table(&column.schema, &column.table),
                    );
                    plan.push(Projection::Column(i));
                }
            };

        for item in &select.projection {
            match item {
                SelectItem::Wildcard(_) => push_all(&mut columns, &mut plan, None),
                SelectItem::QualifiedWildcard(kind, _) => {
                    let name = kind.to_string();
                    let table = name.trim_end_matches(".*").rsplit('.').next().unwrap_or("");
                    push_all(&mut columns, &mut plan, Some(table));
                }
                SelectItem::UnnamedExpr(e) => {
                    let (column, projection) = self.plan_expr(e, None, relation)?;
                    columns.push(column);
                    plan.push(projection);
                }
                SelectItem::ExprWithAlias { expr: e, alias } => {
                    let (column, projection) =
                        self.plan_expr(e, Some(alias.value.clone()), relation)?;
                    columns.push(column);
                    plan.push(projection);
                }
                other => return Err(unsupported(format!("select item `{other}`"))),
            }
        }

        let mut rows = Vec::with_capacity(matching.len());
        for row in matching {
            let ctx = Context {
                row: Some(Row {
                    schema: refs,
                    values: row,
                }),
                session,
            };
            let mut out = Vec::with_capacity(plan.len());
            for step in &plan {
                out.push(match step {
                    Projection::Column(i) => row.get(*i).cloned().unwrap_or(Value::Null),
                    Projection::Expr(e) => self.eval_scalar(session, e, &ctx)?,
                });
            }
            rows.push(out);
        }

        Ok(ResultSet { columns, rows })
    }

    /// Decides how one projected item is produced, and what its metadata is.
    fn plan_expr(
        &self,
        e: &Expr,
        alias: Option<String>,
        relation: &Relation,
    ) -> EngineResult<(OutColumn, Projection)> {
        // A bare column reference keeps the column's declared type, which
        // matters: clients decode a result by the type the metadata claims.
        let direct = match e {
            Expr::Identifier(ident) => relation.index_of(None, &ident.value),
            Expr::CompoundIdentifier(parts) => {
                let name = parts.last().map(|p| p.value.as_str()).unwrap_or_default();
                let table = parts
                    .get(parts.len().wrapping_sub(2))
                    .map(|p| p.value.as_str());
                relation
                    .index_of(table, name)
                    .or_else(|| relation.index_of(None, name))
            }
            _ => None,
        };

        if let Some(i) = direct {
            let column = &relation.columns[i];
            let out = OutColumn::from_sql_type(
                alias.unwrap_or_else(|| column.name.clone()),
                &column.ty,
                column.nullable,
            )
            .in_table(&column.schema, &column.table);
            return Ok((out, Projection::Column(i)));
        }

        let name = alias.unwrap_or_else(|| render_expr(e));
        Ok((
            OutColumn::new(name, ColumnType::VarString),
            Projection::Expr(Box::new(e.clone())),
        ))
    }

    fn aggregate(
        &self,
        session: &Session,
        select: &Select,
        refs: &[ColumnRef],
        matching: &[&Vec<Value>],
    ) -> EngineResult<ResultSet> {
        let mut columns = Vec::new();
        let mut row = Vec::new();

        for item in &select.projection {
            let (alias, e) = match item {
                SelectItem::UnnamedExpr(e) => (render_expr(e), e),
                SelectItem::ExprWithAlias { expr, alias } => (alias.value.clone(), expr),
                other => {
                    return Err(unsupported(format!(
                        "select item `{other}` with aggregates"
                    )))
                }
            };

            let Expr::Function(f) = e else {
                // Mixing bare columns with aggregates needs GROUP BY semantics
                // that are not implemented; ONLY_FULL_GROUP_BY rejects it too.
                return Err(unsupported("non-aggregate columns alongside aggregates"));
            };

            let name = f.name.to_string().to_ascii_lowercase();
            let argument = aggregate_argument(f);
            let value = self.fold(session, &name, argument.as_ref(), refs, matching)?;
            columns.push(OutColumn::new(alias, wire_type(&value)));
            row.push(value);
        }

        Ok(ResultSet {
            columns,
            rows: vec![row],
        })
    }

    /// Folds one aggregate over the matching rows.
    fn fold(
        &self,
        session: &Session,
        name: &str,
        argument: Option<&Expr>,
        refs: &[ColumnRef],
        matching: &[&Vec<Value>],
    ) -> EngineResult<Value> {
        let mut values = Vec::new();
        if let Some(arg) = argument {
            for row in matching {
                let ctx = Context {
                    row: Some(Row {
                        schema: refs,
                        values: row,
                    }),
                    session,
                };
                values.push(expr::eval(arg, &ctx)?);
            }
        }

        // Aggregates ignore NULL, except COUNT(*) which counts rows.
        let present: Vec<&Value> = values.iter().filter(|v| !v.is_null()).collect();

        Ok(match name {
            "count" => match argument {
                None => Value::Int(matching.len() as i64),
                Some(_) => Value::Int(present.len() as i64),
            },
            "sum" => {
                if present.is_empty() {
                    Value::Null
                } else if present.iter().all(|v| matches!(v, Value::Int(_))) {
                    Value::Int(present.iter().filter_map(|v| v.as_int()).sum())
                } else {
                    Value::Float(present.iter().filter_map(|v| v.as_float()).sum())
                }
            }
            "avg" => {
                if present.is_empty() {
                    Value::Null
                } else {
                    let total: f64 = present.iter().filter_map(|v| v.as_float()).sum();
                    Value::Float(total / present.len() as f64)
                }
            }
            "min" | "max" => {
                let want_max = name == "max";
                present
                    .into_iter()
                    .cloned()
                    .reduce(|a, b| {
                        let take_b = match a.compare(&b) {
                            Some(std::cmp::Ordering::Less) => want_max,
                            Some(std::cmp::Ordering::Greater) => !want_max,
                            _ => false,
                        };
                        if take_b {
                            b
                        } else {
                            a
                        }
                    })
                    .unwrap_or(Value::Null)
            }
            "group_concat" => {
                if present.is_empty() {
                    Value::Null
                } else {
                    Value::Text(
                        present
                            .iter()
                            .map(|v| v.as_text())
                            .collect::<Vec<_>>()
                            .join(","),
                    )
                }
            }
            other => return Err(unsupported(format!("aggregate `{other}()`"))),
        })
    }

    /// Sorts an already-projected result, for ORDER BY over output aliases.
    fn order_projected(
        &self,
        result: &mut ResultSet,
        session: &Session,
        order: Option<&OrderBy>,
    ) -> EngineResult<()> {
        let Some(order) = order else { return Ok(()) };
        let OrderByKind::Expressions(items) = &order.kind else {
            return Err(unsupported("this ORDER BY form"));
        };

        // Sorting happens over the projected rows, so ORDER BY may only
        // reference output columns. That covers what Prisma emits.
        let refs: Vec<ColumnRef> = result
            .columns
            .iter()
            .map(|c| ColumnRef::new(&c.table, &c.name))
            .collect();

        let mut keyed: Vec<(Vec<Value>, Vec<Value>)> = Vec::with_capacity(result.rows.len());
        for row in &result.rows {
            let ctx = Context {
                row: Some(Row {
                    schema: &refs,
                    values: row,
                }),
                session,
            };
            let mut key = Vec::with_capacity(items.len());
            for item in items {
                key.push(expr::eval(&item.expr, &ctx)?);
            }
            keyed.push((key, row.clone()));
        }

        let descending: Vec<bool> = items.iter().map(|i| i.options.asc == Some(false)).collect();

        keyed.sort_by(|a, b| compare_keys(&a.0, &b.0, &descending));

        result.rows = keyed.into_iter().map(|(_, row)| row).collect();
        Ok(())
    }

    // ---------------------------------------------------------------- writes

    fn insert(&self, session: &mut Session, insert: &Insert) -> EngineResult<Outcome> {
        let rendered = insert.table.to_string();
        let (db, table_name) = self.split_parts(session, &rendered)?;
        let ignore = insert.ignore;

        let source_rows = match &insert.source {
            Some(query) => match query.body.as_ref() {
                SetExpr::Values(values) => values.rows.clone(),
                _ => return Err(unsupported("INSERT ... SELECT")),
            },
            None => return Err(unsupported("INSERT without VALUES")),
        };

        let session_snapshot = session.clone();
        let mut catalog = self.catalog_mut()?;
        let database = catalog
            .database_mut(&db)
            .ok_or_else(|| EngineError::unknown_database(&db))?;
        let table = database
            .table_mut(&table_name)
            .ok_or_else(|| EngineError::no_such_table(&table_name))?;

        // Named columns, or every column in declaration order.
        let targets: Vec<usize> = if insert.columns.is_empty() {
            (0..table.columns.len()).collect()
        } else {
            insert
                .columns
                .iter()
                .map(|c| {
                    let name = c.to_string().trim_matches('`').to_owned();
                    table
                        .column_index(&name)
                        .ok_or_else(|| EngineError::unknown_column(&name, "field list"))
                })
                .collect::<EngineResult<_>>()?
        };

        let mut inserted = 0u64;
        let mut last_id = 0u64;

        for values in &source_rows {
            // MySQL rejects a row whose value count does not match the column
            // list. Truncating instead stores corrupt data for a client bug
            // that would have failed loudly against the real server.
            if values.content.len() != targets.len() {
                return Err(EngineError::new(
                    1136,
                    "21S01",
                    format!(
                        "Column count doesn't match value count at row {}",
                        inserted + 1
                    ),
                ));
            }

            let ctx = Context::rowless(&session_snapshot);
            let mut row = default_row(table, &ctx)?;

            for (slot, value_expr) in targets.iter().zip(values.content.iter()) {
                // DEFAULT in a VALUES list keeps the column's default.
                if matches!(value_expr, Expr::Identifier(i) if i.value.eq_ignore_ascii_case("default"))
                {
                    continue;
                }
                let value = expr::eval(value_expr, &ctx)?;
                let column = &table.columns[*slot];
                row[*slot] = column.ty.coerce(value, &column.name)?;
            }

            for (i, column) in table.columns.iter().enumerate() {
                if column.auto_increment {
                    if row[i].is_null() {
                        row[i] = Value::Int(table.next_auto_increment);
                        last_id = table.next_auto_increment as u64;
                        table.next_auto_increment += 1;
                    } else if let Some(explicit) = row[i].as_int() {
                        // An explicit value moves the counter past it, as
                        // MySQL does. Without this, a fixture that inserts
                        // explicit ids collides with generated ones later.
                        table.next_auto_increment = table.next_auto_increment.max(explicit + 1);
                    }
                }
                if !column.nullable && row[i].is_null() {
                    return Err(EngineError::null_not_allowed(&column.name));
                }
            }

            match table.check_unique(&row, None) {
                Ok(()) => {}
                // INSERT IGNORE turns a constraint violation into a skip.
                Err(_) if ignore => continue,
                Err(e) => return Err(e),
            }

            table.rows.push(row);
            inserted += 1;
        }

        session.last_insert_id = last_id;
        session.affected_rows = inserted;
        Ok(Outcome::Affected {
            rows: inserted,
            last_insert_id: last_id,
        })
    }

    fn update(
        &self,
        session: &mut Session,
        table: &TableWithJoins,
        assignments: &[Assignment],
        selection: Option<&Expr>,
    ) -> EngineResult<Outcome> {
        if !table.joins.is_empty() {
            return Err(unsupported("multi-table UPDATE"));
        }
        let TableFactor::Table { name, .. } = &table.relation else {
            return Err(unsupported("this UPDATE target"));
        };
        let (db, table_name) = self.split_name(session, name)?;

        let session_snapshot = session.clone();
        let mut catalog = self.catalog_mut()?;
        let database = catalog
            .database_mut(&db)
            .ok_or_else(|| EngineError::unknown_database(&db))?;
        let table = database
            .table_mut(&table_name)
            .ok_or_else(|| EngineError::no_such_table(&table_name))?;

        let refs: Vec<ColumnRef> = table
            .columns
            .iter()
            .map(|c| ColumnRef::new(&table.name, &c.name))
            .collect();

        let mut updated = 0u64;
        let mut rows = table.rows.clone();

        for index in 0..rows.len() {
            let ctx = Context {
                row: Some(Row {
                    schema: &refs,
                    values: &rows[index],
                }),
                session: &session_snapshot,
            };
            let matched = match selection {
                Some(p) => expr::eval(p, &ctx)?.truthy() == Some(true),
                None => true,
            };
            if !matched {
                continue;
            }

            let mut candidate = rows[index].clone();
            for assignment in assignments {
                let column_name = assignment_column(&assignment.target)?;
                let slot = table
                    .column_index(&column_name)
                    .ok_or_else(|| EngineError::unknown_column(&column_name, "field list"))?;
                // The right-hand side sees the row as it was before this
                // statement, so `SET a = b, b = a` swaps rather than copies.
                let value = expr::eval(&assignment.value, &ctx)?;
                let column = &table.columns[slot];
                let coerced = column.ty.coerce(value, &column.name)?;
                if !column.nullable && coerced.is_null() {
                    return Err(EngineError::null_not_allowed(&column.name));
                }
                candidate[slot] = coerced;
            }

            table.check_unique_against(&rows, &candidate, index)?;
            rows[index] = candidate;
            updated += 1;
        }

        table.rows = rows;
        session.affected_rows = updated;
        Ok(Outcome::affected(updated))
    }

    fn delete(
        &self,
        session: &mut Session,
        delete: &sqlparser::ast::Delete,
    ) -> EngineResult<Outcome> {
        let from = match &delete.from {
            sqlparser::ast::FromTable::WithFromKeyword(t)
            | sqlparser::ast::FromTable::WithoutKeyword(t) => t,
        };
        if from.len() != 1 || !from[0].joins.is_empty() {
            return Err(unsupported("multi-table DELETE"));
        }
        let TableFactor::Table { name, .. } = &from[0].relation else {
            return Err(unsupported("this DELETE target"));
        };
        let (db, table_name) = self.split_name(session, name)?;

        let session_snapshot = session.clone();
        let mut catalog = self.catalog_mut()?;
        let database = catalog
            .database_mut(&db)
            .ok_or_else(|| EngineError::unknown_database(&db))?;
        let table = database
            .table_mut(&table_name)
            .ok_or_else(|| EngineError::no_such_table(&table_name))?;

        let refs: Vec<ColumnRef> = table
            .columns
            .iter()
            .map(|c| ColumnRef::new(&table.name, &c.name))
            .collect();

        let before = table.rows.len();
        let mut kept = Vec::with_capacity(before);
        for row in std::mem::take(&mut table.rows) {
            let ctx = Context {
                row: Some(Row {
                    schema: &refs,
                    values: &row,
                }),
                session: &session_snapshot,
            };
            let matched = match &delete.selection {
                Some(p) => expr::eval(p, &ctx)?.truthy() == Some(true),
                None => true,
            };
            if !matched {
                kept.push(row);
            }
        }

        let removed = (before - kept.len()) as u64;
        table.rows = kept;
        session.affected_rows = removed;
        Ok(Outcome::affected(removed))
    }

    fn drop(
        &self,
        session: &mut Session,
        object_type: ObjectType,
        names: &[ObjectName],
        if_exists: bool,
    ) -> EngineResult<Outcome> {
        match object_type {
            ObjectType::Database | ObjectType::Schema => {
                for name in names {
                    self.catalog_mut()?
                        .drop_database(&object_name(name), if_exists)?;
                }
            }
            ObjectType::Table => {
                for name in names {
                    let (db, table) = self.split_name(session, name)?;
                    let mut catalog = self.catalog_mut()?;
                    let Some(database) = catalog.database_mut(&db) else {
                        if if_exists {
                            continue;
                        }
                        return Err(EngineError::unknown_database(&db));
                    };
                    let key = database
                        .tables
                        .keys()
                        .find(|k| k.eq_ignore_ascii_case(&table))
                        .cloned();
                    match key {
                        Some(k) => {
                            database.tables.remove(&k);
                        }
                        None if if_exists => {}
                        None => return Err(EngineError::no_such_table(&table)),
                    }
                }
            }
            // DROP EVENT IF EXISTS appears in the migrations against an event
            // that no longer exists; accepting it as a no-op is enough.
            ObjectType::Index if if_exists => {}
            other => return Err(unsupported(format!("DROP {other}"))),
        }
        Ok(Outcome::ok())
    }

    fn set(&self, session: &mut Session, set: &sqlparser::ast::Set) -> EngineResult<Outcome> {
        use sqlparser::ast::Set as S;
        match set {
            S::SingleAssignment {
                variable, values, ..
            } => {
                let name = object_name(variable);
                let ctx = Context::rowless(session);
                let value = match values.first() {
                    Some(e) => expr::eval(e, &ctx).unwrap_or(Value::Null),
                    None => Value::Null,
                };
                self.assign(session, &name, value);
                Ok(Outcome::ok())
            }
            S::MultipleAssignments { assignments } => {
                for assignment in assignments {
                    let name = object_name(&assignment.name);
                    let ctx = Context::rowless(session);
                    let value = expr::eval(&assignment.value, &ctx).unwrap_or(Value::Null);
                    self.assign(session, &name, value);
                }
                Ok(Outcome::ok())
            }
            // SET NAMES, SET TRANSACTION and friends need no state here.
            _ => Ok(Outcome::ok()),
        }
    }

    fn assign(&self, session: &mut Session, name: &str, value: Value) {
        match name.strip_prefix("@@") {
            Some(rest) => session.set_system_variable(rest, value.as_text()),
            None => match name.strip_prefix('@') {
                Some(rest) => session.set_user_variable(rest, value),
                // A bare name in SET is a system variable.
                None => session.set_system_variable(name, value.as_text()),
            },
        }
    }

    // ----------------------------------------------------------------- SHOW

    fn show_variables(&self, session: &Session) -> ResultSet {
        let mut result = ResultSet::new(vec![
            OutColumn::new("Variable_name", ColumnType::VarString),
            OutColumn::new("Value", ColumnType::VarString),
        ]);
        for (name, value) in session.known_system_variables() {
            result
                .rows
                .push(vec![Value::Text(name), Value::Text(value)]);
        }
        result
    }

    fn show_databases(&self) -> EngineResult<ResultSet> {
        let mut result = ResultSet::new(vec![OutColumn::new("Database", ColumnType::VarString)]);
        result.rows.push(vec![Value::text(INFORMATION_SCHEMA)]);
        for name in self.catalog()?.database_names() {
            result.rows.push(vec![Value::Text(name)]);
        }
        Ok(result)
    }

    fn show_tables(&self, session: &Session) -> EngineResult<ResultSet> {
        let catalog = self.catalog()?;
        let database = catalog.resolve(None, session.current_database.as_deref())?;
        let mut result = ResultSet::new(vec![OutColumn::new(
            format!("Tables_in_{}", database.name),
            ColumnType::VarString,
        )]);
        for name in database.tables.keys() {
            result.rows.push(vec![Value::Text(name.clone())]);
        }
        Ok(result)
    }

    fn show_columns(&self, session: &Session, table: &str) -> EngineResult<ResultSet> {
        let catalog = self.catalog()?;
        let database = catalog.resolve(None, session.current_database.as_deref())?;
        let table = database
            .table(table)
            .ok_or_else(|| EngineError::no_such_table(table))?;

        let mut result = ResultSet::new(vec![
            OutColumn::new("Field", ColumnType::VarString),
            OutColumn::new("Type", ColumnType::VarString),
            OutColumn::new("Null", ColumnType::VarString),
            OutColumn::new("Key", ColumnType::VarString),
            OutColumn::new("Default", ColumnType::VarString),
            OutColumn::new("Extra", ColumnType::VarString),
        ]);

        for column in &table.columns {
            result.rows.push(vec![
                Value::Text(column.name.clone()),
                Value::Text(column.ty.declared.clone()),
                Value::text(if column.nullable { "YES" } else { "NO" }),
                Value::text(if column.primary_key {
                    "PRI"
                } else if column.unique {
                    "UNI"
                } else {
                    ""
                }),
                column.default.clone().unwrap_or(Value::Null),
                Value::text(if column.auto_increment {
                    "auto_increment"
                } else {
                    ""
                }),
            ]);
        }
        Ok(result)
    }

    // ------------------------------------------------------------- internals

    /// Splits a rendered `db.table` string, for the paths where the AST hands
    /// back a name already stringified.
    fn split_parts(&self, session: &Session, rendered: &str) -> EngineResult<(String, String)> {
        let parts: Vec<String> = rendered
            .split('.')
            .map(|p| p.trim_matches('`').to_owned())
            .collect();
        match parts.len() {
            1 => {
                let db = session
                    .current_database
                    .clone()
                    .ok_or_else(EngineError::no_database)?;
                Ok((db, parts[0].clone()))
            }
            2 => Ok((parts[0].clone(), parts[1].clone())),
            _ => Err(unsupported(format!("qualified name `{rendered}`"))),
        }
    }

    /// Splits `db.table` into parts, falling back to the session's database.
    fn split_name(&self, session: &Session, name: &ObjectName) -> EngineResult<(String, String)> {
        let parts: Vec<String> = name
            .0
            .iter()
            .map(|p| p.to_string().trim_matches('`').to_owned())
            .collect();

        match parts.len() {
            1 => {
                let db = session
                    .current_database
                    .clone()
                    .ok_or_else(EngineError::no_database)?;
                Ok((db, parts[0].clone()))
            }
            2 => Ok((parts[0].clone(), parts[1].clone())),
            _ => Err(unsupported(format!("qualified name `{name}`"))),
        }
    }
}

/// Builds the equality condition a `USING (a, b)` join clause implies.
fn using_condition(
    names: &[sqlparser::ast::ObjectName],
    left: &Relation,
    right: &Relation,
) -> EngineResult<Expr> {
    use sqlparser::ast::{BinaryOperator, Ident};

    let mut condition: Option<Expr> = None;
    for name in names {
        let column = name.to_string().trim_matches('`').to_owned();
        let (Some(l), Some(r)) = (left.index_of(None, &column), right.index_of(None, &column))
        else {
            return Err(EngineError::unknown_column(&column, "from clause"));
        };
        let eq = Expr::BinaryOp {
            left: Box::new(Expr::CompoundIdentifier(vec![
                Ident::new(left.columns[l].table.clone()),
                Ident::new(column.clone()),
            ])),
            op: BinaryOperator::Eq,
            right: Box::new(Expr::CompoundIdentifier(vec![
                Ident::new(right.columns[r].table.clone()),
                Ident::new(column),
            ])),
        };
        condition = Some(match condition {
            None => eq,
            Some(existing) => Expr::BinaryOp {
                left: Box::new(existing),
                op: BinaryOperator::And,
                right: Box::new(eq),
            },
        });
    }
    condition.ok_or_else(|| EngineError::parse("USING clause names no columns"))
}

/// A result together with whether its ORDER BY still needs applying.
struct Ordered {
    result: ResultSet,
    pending_order: bool,
}

impl Ordered {
    fn sorted(result: ResultSet) -> Self {
        Self {
            result,
            pending_order: false,
        }
    }
}

impl std::ops::Deref for Ordered {
    type Target = ResultSet;
    fn deref(&self) -> &ResultSet {
        &self.result
    }
}

/// Compares two sort keys, honouring per-column direction.
///
/// NULLs sort first ascending, which is what MySQL does and what a client
/// paginating over a nullable column depends on.
fn compare_keys(a: &[Value], b: &[Value], descending: &[bool]) -> std::cmp::Ordering {
    for (i, desc) in descending.iter().enumerate() {
        let (Some(left), Some(right)) = (a.get(i), b.get(i)) else {
            continue;
        };
        let ord = match (left.is_null(), right.is_null()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (false, false) => left.compare(right).unwrap_or(std::cmp::Ordering::Equal),
        };
        let ord = if *desc { ord.reverse() } else { ord };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

/// How one projected column is produced.
enum Projection {
    /// Copied straight from the stored row.
    Column(usize),
    /// Boxed: an Expr is far larger than an index, and a projection list is
    /// mostly plain columns.
    Expr(Box<Expr>),
}

impl Table {
    /// Uniqueness check against an explicit row set, used by UPDATE where the
    /// candidate rows are not yet committed to the table.
    fn check_unique_against(
        &self,
        rows: &[Vec<Value>],
        candidate: &[Value],
        skip: usize,
    ) -> EngineResult<()> {
        let mut probe = self.clone();
        probe.rows = rows.to_vec();
        probe.check_unique(candidate, Some(skip))
    }
}

/// Builds a row of column defaults.
fn default_row(table: &Table, ctx: &Context<'_>) -> EngineResult<Vec<Value>> {
    let mut row = Vec::with_capacity(table.columns.len());
    for column in &table.columns {
        let value = match (&column.default, &column.default_expr) {
            (Some(v), _) => v.clone(),
            // Expression defaults are evaluated per row, so every inserted row
            // gets its own timestamp rather than sharing the DDL's.
            (None, Some(e)) => {
                let parsed = Parser::new(&MySqlDialect {})
                    .try_with_sql(e)
                    .and_then(|mut p| p.parse_expr())
                    .map_err(|err| EngineError::parse(format!("{err}")))?;
                expr::eval(&parsed, ctx)?
            }
            (None, None) => Value::Null,
        };
        row.push(value);
    }
    Ok(row)
}

fn apply_limit(result: &mut ResultSet, session: &Session, query: &Query) -> EngineResult<()> {
    let ctx = Context::rowless(session);

    if let Some(clause) = &query.limit_clause {
        use sqlparser::ast::LimitClause;
        let (limit, offset) = match clause {
            LimitClause::LimitOffset { limit, offset, .. } => (
                limit.as_ref().map(|e| expr::eval(e, &ctx)).transpose()?,
                offset
                    .as_ref()
                    .map(|o| expr::eval(&o.value, &ctx))
                    .transpose()?,
            ),
            LimitClause::OffsetCommaLimit { offset, limit } => (
                Some(expr::eval(limit, &ctx)?),
                Some(expr::eval(offset, &ctx)?),
            ),
        };

        if let Some(offset) = offset.and_then(|v| v.as_int()) {
            let skip = offset.max(0) as usize;
            result.rows = result.rows.split_off(skip.min(result.rows.len()));
        }
        if let Some(limit) = limit.and_then(|v| v.as_int()) {
            result.rows.truncate(limit.max(0) as usize);
        }
    }

    Ok(())
}

/// Removes duplicate rows for SELECT DISTINCT.
///
/// Silently ignoring DISTINCT returns a plausible-looking result with the wrong
/// number of rows, which is exactly the failure a test suite cannot detect.
fn apply_distinct(
    result: &mut ResultSet,
    distinct: Option<&sqlparser::ast::Distinct>,
) -> EngineResult<()> {
    match distinct {
        None => Ok(()),
        Some(sqlparser::ast::Distinct::Distinct) => {
            // Order is preserved, so a preceding ORDER BY still holds.
            let mut seen: Vec<Vec<String>> = Vec::new();
            let mut kept = Vec::with_capacity(result.rows.len());
            for row in std::mem::take(&mut result.rows) {
                // NULL is its own distinct value, and must not collide with
                // the empty string.
                let key: Vec<String> = row
                    .iter()
                    .map(|v| {
                        if v.is_null() {
                            "\u{0}NULL".to_owned()
                        } else {
                            v.as_text()
                        }
                    })
                    .collect();
                if !seen.contains(&key) {
                    seen.push(key);
                    kept.push(row);
                }
            }
            result.rows = kept;
            Ok(())
        }
        Some(other) => Err(unsupported(format!("`{other}`"))),
    }
}

fn projection_is_aggregate(projection: &[SelectItem]) -> bool {
    projection.iter().any(|item| {
        let e = match item {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
            _ => return false,
        };
        matches!(e, Expr::Function(f) if is_aggregate(&f.name.to_string()))
    })
}

fn is_aggregate(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "count" | "sum" | "avg" | "min" | "max" | "group_concat"
    )
}

/// The single argument of an aggregate, or `None` for `COUNT(*)`.
fn aggregate_argument(f: &sqlparser::ast::Function) -> Option<Expr> {
    use sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArguments};
    let FunctionArguments::List(list) = &f.args else {
        return None;
    };
    list.args.iter().find_map(|arg| match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e.clone()),
        _ => None,
    })
}

fn assignment_column(target: &AssignmentTarget) -> EngineResult<String> {
    match target {
        AssignmentTarget::ColumnName(name) => Ok(name
            .0
            .last()
            .map(|p| p.to_string().trim_matches('`').to_owned())
            .unwrap_or_default()),
        other => Err(unsupported(format!("assignment target `{other}`"))),
    }
}

fn object_name(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|p| p.to_string().trim_matches('`').to_owned())
        .collect::<Vec<_>>()
        .join(".")
}

/// The column name a client sees for an unaliased expression: MySQL echoes the
/// expression text back.
fn render_expr(e: &Expr) -> String {
    match e {
        Expr::Identifier(i) => i.value.clone(),
        Expr::CompoundIdentifier(parts) => {
            parts.last().map(|p| p.value.clone()).unwrap_or_default()
        }
        other => other.to_string(),
    }
}

fn wire_type(value: &Value) -> ColumnType {
    match value {
        Value::Int(_) => ColumnType::LongLong,
        Value::Float(_) => ColumnType::Double,
        Value::Bytes(_) => ColumnType::Blob,
        _ => ColumnType::VarString,
    }
}

fn first_words(s: &str, n: usize) -> String {
    s.split_whitespace().take(n).collect::<Vec<_>>().join(" ")
}

fn poisoned() -> EngineError {
    EngineError::new(
        1105,
        "HY000",
        "engine state was poisoned by a previous panic",
    )
}
