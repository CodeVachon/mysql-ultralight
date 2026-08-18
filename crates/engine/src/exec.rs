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

use crate::catalog::{Catalog, Database, Table, INFORMATION_SCHEMA};
use crate::ddl;
use crate::error::{unsupported, EngineError, EngineResult};
use crate::expr::{self, ColumnRef, Context, OuterRow, Row};
use crate::foreign_keys::{self, Action};
use crate::info_schema;
use crate::params;
use crate::relation::Relation;
use crate::result::{OutColumn, Outcome, ResultSet};
use crate::rewrite::{self, Intercepted};
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

    /// Lets the DDL module record what it changed, for rollback.
    pub(crate) fn mark_modified_public(&self, session: &mut Session, database: &str, table: &str) {
        mark_modified(session, database, std::slice::from_ref(&table.to_owned()));
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
        self.execute_at_depth(session, sql, parameters, 0)
    }

    /// `depth` bounds recursion through `EXECUTE`, which can otherwise run a
    /// prepared statement that prepares and executes itself.
    fn execute_at_depth(
        &self,
        session: &mut Session,
        sql: &str,
        parameters: &[Value],
        depth: usize,
    ) -> EngineResult<Outcome> {
        const MAX_DEPTH: usize = 8;
        if depth > MAX_DEPTH {
            return Err(EngineError::new(
                1456,
                "HY000",
                "Recursive limit exceeded for prepared statements",
            ));
        }

        let bound = params::bind(sql, parameters);
        let mode = session.sql_mode();

        // Statements the parser cannot take are recognised before it sees them.
        if let Some(intercepted) = rewrite::intercept(&bound) {
            return self.run_intercepted(session, intercepted, depth);
        }

        let normalised = rewrite::normalise(&bound, mode);
        let statements = Parser::parse_sql(&MySqlDialect {}, &normalised)
            .map_err(|e| EngineError::parse(format!("{e}")))?;

        match statements.len() {
            0 => Ok(Outcome::ok()),
            1 => self.run(session, &statements[0]),
            // Multi-statement requires MORE_RESULTS_EXISTS on the wire, which
            // the server does not advertise. Erroring beats returning only the
            // first result and letting the caller believe it ran everything.
            _ => Err(unsupported("multiple statements in one command")),
        }
    }

    /// Runs a statement recognised outside the general parser.
    fn run_intercepted(
        &self,
        session: &mut Session,
        intercepted: Intercepted,
        depth: usize,
    ) -> EngineResult<Outcome> {
        match intercepted {
            Intercepted::Prepare { name, source } => {
                // The source is either a literal or a user variable holding
                // one. The migrations build it with IF(), so the variable form
                // is the one that matters.
                let text = match source.strip_prefix('@') {
                    Some(variable) => {
                        let value = session.user_variable(variable);
                        if value.is_null() {
                            return Err(EngineError::new(
                                1064,
                                "42000",
                                format!("Prepared statement source @{variable} is NULL"),
                            ));
                        }
                        value.as_text()
                    }
                    None => source.trim_matches('\'').to_owned(),
                };
                session.prepared.insert(name.to_ascii_lowercase(), text);
                Ok(Outcome::ok())
            }

            Intercepted::Execute { name, using } => {
                let sql = session
                    .prepared
                    .get(&name.to_ascii_lowercase())
                    .cloned()
                    .ok_or_else(|| {
                        EngineError::new(
                            1243,
                            "HY000",
                            format!("Unknown prepared statement handler ({name}) given to EXECUTE"),
                        )
                    })?;

                let parameters: Vec<Value> = using
                    .iter()
                    .map(|v| session.user_variable(v.trim_start_matches('@')))
                    .collect();

                self.execute_at_depth(session, &sql, &parameters, depth + 1)
            }

            Intercepted::Deallocate { name } => {
                session.prepared.remove(&name.to_ascii_lowercase());
                Ok(Outcome::ok())
            }

            Intercepted::RenameIndex { table, old, new } => {
                let (db, table_name) = self.split_parts(session, &table)?;
                let mut catalog = self.catalog_mut()?;
                let database = catalog
                    .database_mut(&db)
                    .ok_or_else(|| EngineError::unknown_database(&db))?;
                let table = database
                    .table_mut(&table_name)
                    .ok_or_else(|| EngineError::no_such_table(&table_name))?;

                match table
                    .indexes
                    .iter_mut()
                    .find(|i| i.name.eq_ignore_ascii_case(&old))
                {
                    Some(index) => index.name = new,
                    None => {
                        return Err(EngineError::new(
                            1176,
                            "42000",
                            format!("Key '{old}' doesn't exist in table '{table_name}'"),
                        ))
                    }
                }
                Ok(Outcome::ok())
            }

            // No CREATE EVENT exists anywhere in the target codebase; the one
            // DROP is against a legacy event, so accepting it is enough.
            Intercepted::DropEvent { .. } => Ok(Outcome::ok()),
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
            Statement::CreateView(create) => {
                let (db, name) = self.split_name(session, &create.name)?;
                let explicit: Vec<String> = create
                    .columns
                    .iter()
                    .map(|c| c.name.value.clone())
                    .collect();

                // Running the definition now both validates it and fixes the
                // view's shape, which information_schema needs later without a
                // session to run anything against.
                let mut scoped = session.clone();
                scoped.current_database = Some(db.clone());
                let resolved = self.query(&scoped, &create.query)?;
                let shape: Vec<crate::catalog::Column> = resolved
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let mut column = crate::catalog::Column::new(
                            explicit.get(i).cloned().unwrap_or_else(|| c.name.clone()),
                            crate::types::SqlType::from_column_type(c.ty),
                        );
                        column.position = i + 1;
                        column.nullable = c.nullable;
                        column
                    })
                    .collect();

                let view = crate::catalog::View {
                    name: name.clone(),
                    definition: create.query.to_string(),
                    columns: explicit,
                    shape,
                };

                let mut catalog = self.catalog_mut()?;
                let database = catalog
                    .database_mut(&db)
                    .ok_or_else(|| EngineError::unknown_database(&db))?;

                if !create.or_replace && database.view(&name).is_some() {
                    return Err(EngineError::table_exists(&name));
                }
                // A view and a table cannot share a name.
                if database.table(&name).is_some() {
                    return Err(EngineError::table_exists(&name));
                }
                database.views.insert(name, view);
                Ok(Outcome::ok())
            }
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

            Statement::StartTransaction { .. } => {
                // A nested BEGIN commits the open transaction in MySQL rather
                // than nesting, so the snapshot is simply replaced.
                session.transaction = Some(crate::session::Transaction {
                    opened: self.catalog()?.clone(),
                    savepoints: Vec::new(),
                    modified: Vec::new(),
                });
                session.in_transaction = true;
                Ok(Outcome::ok())
            }
            Statement::Commit { .. } => {
                session.transaction = None;
                session.in_transaction = false;
                Ok(Outcome::ok())
            }
            Statement::Rollback { savepoint, .. } => {
                match savepoint {
                    // ROLLBACK TO SAVEPOINT keeps the transaction open and
                    // discards only the savepoints taken after the target.
                    Some(name) => {
                        let name = name.value.clone();
                        let Some(transaction) = session.transaction.as_mut() else {
                            return Err(EngineError::new(
                                1305,
                                "42000",
                                format!("SAVEPOINT {name} does not exist"),
                            ));
                        };
                        let Some(at) = transaction
                            .savepoints
                            .iter()
                            .rposition(|s| s.name.eq_ignore_ascii_case(&name))
                        else {
                            return Err(EngineError::new(
                                1305,
                                "42000",
                                format!("SAVEPOINT {name} does not exist"),
                            ));
                        };
                        let restored = transaction.savepoints[at].catalog.clone();
                        // Every table the transaction has written is restored
                        // to its state at the savepoint. Restoring only those
                        // written *since* would leave a table modified both
                        // before and after it holding the later value.
                        let undo = transaction.modified.clone();
                        transaction.savepoints.truncate(at + 1);
                        restore_tables(&mut *self.catalog_mut()?, &restored, &undo);
                    }
                    None => {
                        if let Some(transaction) = session.transaction.take() {
                            restore_tables(
                                &mut *self.catalog_mut()?,
                                &transaction.opened,
                                &transaction.modified,
                            );
                        }
                        session.in_transaction = false;
                    }
                }
                Ok(Outcome::ok())
            }
            Statement::Savepoint { name } => {
                let snapshot = self.catalog()?.clone();
                let name = name.value.clone();
                // Under autocommit a savepoint is released at the end of the
                // statement. Opening a transaction here would make every later
                // write rollback-able, and a stray ROLLBACK would undo
                // committed work.
                if let Some(transaction) = session.transaction.as_mut() {
                    // Re-taking a name replaces the earlier savepoint.
                    transaction
                        .savepoints
                        .retain(|s| !s.name.eq_ignore_ascii_case(&name));
                    let modified = transaction.modified.clone();
                    transaction.savepoints.push(crate::session::Savepoint {
                        name,
                        catalog: snapshot,
                        modified,
                    });
                }
                Ok(Outcome::ok())
            }
            Statement::ReleaseSavepoint { name } => {
                if let Some(transaction) = session.transaction.as_mut() {
                    transaction
                        .savepoints
                        .retain(|s| !s.name.eq_ignore_ascii_case(&name.value));
                }
                Ok(Outcome::ok())
            }

            other => Err(unsupported(format!(
                "statement `{}`",
                first_words(&other.to_string(), 3)
            ))),
        }
    }

    // ---------------------------------------------------------------- SELECT

    fn query(&self, session: &Session, query: &Query) -> EngineResult<ResultSet> {
        self.query_with_outer(session, query, None)
    }

    /// Runs a query, optionally against an enclosing row so a correlated
    /// subquery can reference the outer query's columns.
    fn query_with_outer(
        &self,
        session: &Session,
        query: &Query,
        outer: Option<&OuterRow>,
    ) -> EngineResult<ResultSet> {
        let select = match query.body.as_ref() {
            SetExpr::Select(select) => select,
            // A set operation runs both sides and combines them, so it is
            // handled before the single-SELECT path.
            SetExpr::SetOperation { .. } | SetExpr::Query(_) => {
                let mut combined = self.set_expr(session, query.body.as_ref(), outer)?;
                self.order_projected(&mut combined, session, query.order_by.as_ref())?;
                apply_limit(&mut combined, session, query)?;
                return Ok(combined);
            }
            other => return Err(unsupported(format!("query body `{other}`"))),
        };

        let result = self.select(session, select, query.order_by.as_ref(), outer)?;
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

    /// Evaluates a set operation, or a nested query body.
    fn set_expr(
        &self,
        session: &Session,
        body: &SetExpr,
        outer: Option<&OuterRow>,
    ) -> EngineResult<ResultSet> {
        match body {
            SetExpr::Select(select) => Ok(self.select(session, select, None, outer)?.result),
            SetExpr::Query(query) => self.query_with_outer(session, query, outer),
            SetExpr::SetOperation {
                left,
                op,
                set_quantifier,
                right,
            } => {
                use sqlparser::ast::{SetOperator, SetQuantifier};

                // Reject an unsupported operator before running either side,
                // so the error names the real problem rather than whatever the
                // branches happen to hit first.
                match op {
                    SetOperator::Union => {}
                    other => return Err(unsupported(format!("{other}"))),
                }

                let mut left = self.set_expr(session, left, outer)?;
                let right = self.set_expr(session, right, outer)?;

                if left.columns.len() != right.columns.len() {
                    return Err(EngineError::new(
                        1222,
                        "21000",
                        "The used SELECT statements have a different number of columns",
                    ));
                }

                // Column names come from the first branch, as in MySQL.
                left.rows.extend(right.rows);

                // UNION removes duplicates; only UNION ALL keeps them.
                if !matches!(
                    set_quantifier,
                    SetQuantifier::All | SetQuantifier::AllByName
                ) {
                    dedupe(&mut left.rows);
                }
                Ok(left)
            }
            other => Err(unsupported(format!("query body `{other}`"))),
        }
    }

    fn select(
        &self,
        session: &Session,
        select: &Select,
        order: Option<&OrderBy>,
        outer: Option<&OuterRow>,
    ) -> EngineResult<Ordered> {
        // A SELECT with no FROM evaluates its projection once, which is the
        // shape of `SELECT 1` and every driver's preamble query.
        if select.from.is_empty() {
            return Ok(Ordered::sorted(
                self.select_without_table(session, select, outer)?,
            ));
        }
        if select.from.len() > 1 {
            return Err(unsupported("comma-separated FROM items"));
        }

        let relation = self.build_relation(session, &select.from[0])?;
        // The outer row's columns are appended, so an unqualified name resolves
        // against this query's columns first and the enclosing one only if it
        // is not found here, which is MySQL's scoping rule.
        let mut refs = relation.refs();
        if let Some(outer) = outer {
            refs.extend(outer.schema.iter().cloned());
        }

        // Filter first so projections and aggregates see only matching rows.
        // Each candidate row is widened with the outer row's values so both
        // are visible to the predicate.
        // Only a correlated subquery needs widened rows; an ordinary query
        // borrows the relation's rows rather than copying the whole set.
        let widened: Option<Vec<Vec<Value>>> = outer.map(|outer| {
            relation
                .rows
                .iter()
                .map(|row| {
                    let mut combined = row.clone();
                    combined.extend(outer.values.iter().cloned());
                    combined
                })
                .collect()
        });
        let source: &Vec<Vec<Value>> = widened.as_ref().unwrap_or(&relation.rows);

        let mut matching: Vec<&Vec<Value>> = Vec::new();
        for row in source {
            let ctx = Context {
                row: Some(Row {
                    schema: &refs,
                    values: row,
                }),
                session,
                subqueries: Some(self),
            };
            let keep = match &select.selection {
                Some(predicate) => expr::eval(predicate, &ctx)?.truthy() == Some(true),
                None => true,
            };
            if keep {
                matching.push(row);
            }
        }

        let group_exprs = match &select.group_by {
            GroupByExpr::Expressions(exprs, _) => exprs.clone(),
            GroupByExpr::All(_) => return Err(unsupported("GROUP BY ALL")),
        };

        // A query is grouped if it says so, or if it aggregates: an aggregate
        // with no GROUP BY is one group covering every row.
        // An aggregate anywhere in a select item groups the query, not just a
        // bare one: `SELECT SUM(a) / COUNT(*)` and `SELECT IFNULL(SUM(v), 0)`
        // are aggregate queries. HAVING alone groups it too, since MySQL
        // treats the whole result as one group in that case.
        let aggregating = !group_exprs.is_empty()
            || select.having.is_some()
            || select.projection.iter().any(select_item_aggregates);

        if aggregating {
            let mut grouped =
                self.grouped(session, select, &refs, &matching, &group_exprs, order)?;
            apply_distinct(&mut grouped, select.distinct.as_ref())?;
            return Ok(Ordered::sorted(grouped));
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
                subqueries: Some(self),
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
                            subqueries: Some(self),
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

                // A view is a named query, so it is run and its rows stand in
                // for a table's.
                let view = if db.eq_ignore_ascii_case(INFORMATION_SCHEMA) {
                    None
                } else {
                    let catalog = self.catalog()?;
                    catalog
                        .database(&db)
                        .and_then(|d| d.view(&table_name))
                        .cloned()
                };
                if let Some(view) = view {
                    let shown = alias
                        .as_ref()
                        .map(|a| a.name.value.clone())
                        .unwrap_or_else(|| view.name.clone());
                    return self.materialise_view(session, &view, &db, &shown);
                }

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
                            ty: crate::types::SqlType::from_column_type(c.ty),
                            nullable: c.nullable,
                        })
                        .collect(),
                    rows: inner.rows,
                })
            }
            other => Err(unsupported(format!("FROM item `{other}`"))),
        }
    }

    fn select_without_table(
        &self,
        session: &Session,
        select: &Select,
        outer: Option<&OuterRow>,
    ) -> EngineResult<ResultSet> {
        // `SELECT (SELECT a) FROM t` has no FROM of its own but must still see
        // the enclosing row.
        let ctx = match outer {
            None => Context::rowless(session),
            Some(outer) => Context {
                row: Some(Row {
                    schema: &outer.schema,
                    values: &outer.values,
                }),
                session,
                subqueries: None,
            },
        }
        .with_subqueries(self);
        let mut columns = Vec::new();
        let mut row = Vec::new();

        for item in &select.projection {
            let (alias, e) = match item {
                SelectItem::UnnamedExpr(e) => (render_expr(e), e),
                SelectItem::ExprWithAlias { expr, alias } => (alias.value.clone(), expr),
                other => return Err(unsupported(format!("select item `{other}`"))),
            };
            let value = expr::eval(e, &ctx)?;
            columns.push(OutColumn::new(alias, wire_type(&value)));
            row.push(value);
        }

        Ok(ResultSet {
            columns,
            rows: vec![row],
        })
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
        // A window function sees the whole result rather than one row, so its
        // values are computed up front and read back by row index.
        let mut windows: Vec<Vec<Value>> = Vec::new();

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
                SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                    let alias = match item {
                        SelectItem::ExprWithAlias { alias, .. } => Some(alias.value.clone()),
                        _ => None,
                    };

                    if let Expr::Function(f) = e {
                        if f.over.is_some() {
                            let values = self.compute_window(f, refs, matching, session)?;
                            // Typed from the function, not the first value: a
                            // later row may differ, and the binary protocol
                            // encodes by the declared type.
                            columns.push(OutColumn::new(
                                alias.unwrap_or_else(|| render_expr(e)),
                                computed_column_type(e),
                            ));
                            plan.push(Projection::Window(windows.len()));
                            windows.push(values);
                            continue;
                        }
                    }

                    let (column, projection) = self.plan_expr(e, alias, relation)?;
                    columns.push(column);
                    plan.push(projection);
                }
                other => return Err(unsupported(format!("select item `{other}`"))),
            }
        }

        let mut rows = Vec::with_capacity(matching.len());
        for (index, row) in matching.iter().enumerate() {
            let ctx = Context {
                row: Some(Row {
                    schema: refs,
                    values: row,
                }),
                session,
                subqueries: Some(self),
            };
            let mut out = Vec::with_capacity(plan.len());
            for step in &plan {
                out.push(match step {
                    Projection::Column(i) => row.get(*i).cloned().unwrap_or(Value::Null),
                    Projection::Window(i) => windows[*i].get(index).cloned().unwrap_or(Value::Null),
                    Projection::Expr(e) => expr::eval(e, &ctx)?,
                });
            }
            rows.push(out);
        }

        Ok(ResultSet { columns, rows })
    }

    /// Computes a window function's value for every matching row.
    ///
    /// Rows are partitioned, each partition ordered, and the result written
    /// back against the row's original position so the projection reads it by
    /// index.
    fn compute_window(
        &self,
        f: &sqlparser::ast::Function,
        refs: &[ColumnRef],
        matching: &[&Vec<Value>],
        session: &Session,
    ) -> EngineResult<Vec<Value>> {
        use sqlparser::ast::WindowType;

        let spec = match f.over.as_ref() {
            Some(WindowType::WindowSpec(spec)) => spec,
            Some(WindowType::NamedWindow(_)) => return Err(unsupported("named windows")),
            None => return Err(unsupported("a window function without OVER")),
        };
        if spec.window_frame.is_some() {
            return Err(unsupported("window frames"));
        }

        let name = f.name.to_string().to_ascii_lowercase();
        let argument = aggregate_argument(f);

        let evaluate = |e: &Expr, row: &Vec<Value>| -> EngineResult<Value> {
            let ctx = Context {
                row: Some(Row {
                    schema: refs,
                    values: row,
                }),
                session,
                subqueries: Some(self),
            };
            expr::eval(e, &ctx)
        };

        // Partition, keeping each row's original index so results land back
        // where the projection will look for them.
        let mut partition_keys: Vec<Vec<String>> = Vec::new();
        let mut partitions: Vec<Vec<usize>> = Vec::new();
        for (index, row) in matching.iter().enumerate() {
            let mut key = Vec::with_capacity(spec.partition_by.len());
            for e in &spec.partition_by {
                let value = evaluate(e, row)?;
                key.push(if value.is_null() {
                    "\u{0}NULL".to_owned()
                } else {
                    value.collation_key()
                });
            }
            match partition_keys.iter().position(|k| *k == key) {
                Some(at) => partitions[at].push(index),
                None => {
                    partition_keys.push(key);
                    partitions.push(vec![index]);
                }
            }
        }

        let descending: Vec<bool> = spec
            .order_by
            .iter()
            .map(|i| i.options.asc == Some(false))
            .collect();

        let mut out = vec![Value::Null; matching.len()];

        for partition in &partitions {
            // Without ORDER BY the input order stands, as in MySQL.
            let mut ordered: Vec<(Vec<Value>, usize)> = Vec::with_capacity(partition.len());
            for index in partition {
                let mut key = Vec::with_capacity(spec.order_by.len());
                for item in &spec.order_by {
                    key.push(evaluate(&item.expr, matching[*index])?);
                }
                ordered.push((key, *index));
            }
            ordered.sort_by(|a, b| compare_keys(&a.0, &b.0, &descending));

            match name.as_str() {
                "row_number" => {
                    for (position, (_, index)) in ordered.iter().enumerate() {
                        out[*index] = Value::Int(position as i64 + 1);
                    }
                }
                "rank" | "dense_rank" => {
                    let dense = name == "dense_rank";
                    let mut rank = 0i64;
                    let mut previous: Option<Vec<Value>> = None;
                    for (position, (key, index)) in ordered.iter().enumerate() {
                        let tied = previous.as_ref().is_some_and(|p| {
                            compare_keys(p, key, &descending) == std::cmp::Ordering::Equal
                        });
                        if !tied {
                            // RANK leaves a gap after a tie; DENSE_RANK does not.
                            rank = if dense { rank + 1 } else { position as i64 + 1 };
                        }
                        out[*index] = Value::Int(rank);
                        previous = Some(key.clone());
                    }
                }
                // An aggregate with OVER and no frame folds over the whole
                // partition, so every row in it gets the same value.
                other if is_aggregate(other) => {
                    let group: Vec<&Vec<Value>> = partition.iter().map(|i| matching[*i]).collect();
                    let value = self.fold(
                        session,
                        other,
                        argument.as_ref(),
                        refs,
                        &group,
                        aggregate_is_distinct(f),
                    )?;
                    for index in partition {
                        out[*index] = value.clone();
                    }
                }
                other => return Err(unsupported(format!("window function `{other}()`"))),
            }
        }

        Ok(out)
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

    /// Evaluates a grouped query: GROUP BY, HAVING, and aggregates.
    ///
    /// An aggregate with no GROUP BY is one group covering every row, which is
    /// why both go through here.
    fn grouped(
        &self,
        session: &Session,
        select: &Select,
        refs: &[ColumnRef],
        matching: &[&Vec<Value>],
        group_exprs: &[Expr],
        order: Option<&OrderBy>,
    ) -> EngineResult<ResultSet> {
        // `GROUP BY 1` means the first select column, not the literal 1.
        // Treating it as a literal gives every row the same key and collapses
        // the whole table into one group.
        let group_exprs: Vec<Expr> = group_exprs
            .iter()
            .map(|e| resolve_ordinal(e, &select.projection).unwrap_or_else(|| e.clone()))
            .collect();

        let mut keys: Vec<Vec<String>> = Vec::new();
        let mut groups: Vec<Vec<&Vec<Value>>> = Vec::new();

        if group_exprs.is_empty() {
            // Ungrouped aggregation always produces exactly one row, even over
            // no rows at all: `SELECT COUNT(*)` on an empty table returns 0.
            groups.push(matching.to_vec());
        } else {
            for row in matching {
                let ctx = Context {
                    row: Some(Row {
                        schema: refs,
                        values: row,
                    }),
                    session,
                    subqueries: Some(self),
                };
                let mut key = Vec::with_capacity(group_exprs.len());
                for e in &group_exprs {
                    let value = expr::eval(e, &ctx)?;
                    // Grouping treats NULLs as equal to one another, unlike
                    // comparison, so they need a key of their own.
                    key.push(if value.is_null() {
                        "\u{0}NULL".to_owned()
                    } else {
                        value.collation_key()
                    });
                }
                match keys.iter().position(|k| *k == key) {
                    Some(index) => groups[index].push(row),
                    None => {
                        keys.push(key);
                        groups.push(vec![*row]);
                    }
                }
            }
        }

        let items = projected_items(&select.projection)?;

        // Under ONLY_FULL_GROUP_BY a selected column must be grouped by or
        // aggregated. Silently evaluating it against the group's first row
        // returns an arbitrary value where MySQL raises an error.
        if session.sql_mode().only_full_group_by {
            for (_, e) in &items {
                check_full_group_by(e, &group_exprs)?;
            }
        }

        let order_items = match order.map(|o| &o.kind) {
            Some(OrderByKind::Expressions(o)) => o.clone(),
            Some(_) => return Err(unsupported("this ORDER BY form")),
            None => Vec::new(),
        };
        let descending: Vec<bool> = order_items
            .iter()
            .map(|i| i.options.asc == Some(false))
            .collect();

        // Column types cannot be inferred from the first group: a later group
        // may produce a different type, and the binary protocol encodes by the
        // declared type, so a client would silently decode the wrong thing.
        // Computed columns are reported as strings, as the ungrouped path does.
        let columns: Vec<OutColumn> = items
            .iter()
            .map(|(alias, e)| OutColumn::new(alias.clone(), computed_column_type(e)))
            .collect();

        let mut rows: Vec<Vec<Value>> = Vec::new();
        let mut sort_keys: Vec<Vec<Value>> = Vec::new();

        for group in &groups {
            let mut out = Vec::with_capacity(items.len());
            for (_, e) in &items {
                out.push(self.eval_over_group(e, group, refs, session)?);
            }

            // HAVING is evaluated after the projection so it can reference an
            // output alias, which MySQL allows and reporting queries rely on.
            if let Some(having) = &select.having {
                let resolved = substitute_aliases(having, &items, &out);
                if self
                    .eval_over_group(&resolved, group, refs, session)?
                    .truthy()
                    != Some(true)
                {
                    continue;
                }
            }

            if !order_items.is_empty() {
                let mut key = Vec::with_capacity(order_items.len());
                for item in &order_items {
                    // ORDER BY may name an ordinal or an output alias; both
                    // resolve against the projection before evaluation.
                    let by_position = ordinal_of(&item.expr).and_then(|n| out.get(n - 1).cloned());
                    key.push(match by_position {
                        Some(value) => value,
                        None => {
                            let resolved = substitute_aliases(&item.expr, &items, &out);
                            self.eval_over_group(&resolved, group, refs, session)?
                        }
                    });
                }
                sort_keys.push(key);
            }

            rows.push(out);
        }

        if !order_items.is_empty() {
            let mut paired: Vec<(Vec<Value>, Vec<Value>)> =
                sort_keys.into_iter().zip(rows).collect();
            paired.sort_by(|a, b| compare_keys(&a.0, &b.0, &descending));
            rows = paired.into_iter().map(|(_, row)| row).collect();
        }

        Ok(ResultSet { columns, rows })
    }

    /// Evaluates an expression over one group, folding any aggregates first.
    ///
    /// Folded values are bound to hidden columns rather than spliced back into
    /// the syntax tree: rendering a value as a literal loses binary data and
    /// changes its type, so `MAX(blob_column)` would come back as text.
    fn eval_over_group(
        &self,
        e: &Expr,
        group: &[&Vec<Value>],
        refs: &[ColumnRef],
        session: &Session,
    ) -> EngineResult<Value> {
        let mut folded: Vec<Value> = Vec::new();
        let rewritten = self.fold_aggregates(e, group, refs, session, &mut folded)?;

        // Hidden columns carry the folded values, so the rest of the
        // expression evaluates by the ordinary rules.
        let mut schema: Vec<ColumnRef> = refs.to_vec();
        let mut values: Vec<Value> = match group.first() {
            Some(row) => row.to_vec(),
            // An empty group still has to line up with the schema, or the
            // hidden columns land at the wrong index and cannot be resolved.
            None => vec![Value::Null; refs.len()],
        };
        values.resize(refs.len(), Value::Null);
        for (index, value) in folded.into_iter().enumerate() {
            schema.push(ColumnRef::new("", folded_name(index)));
            values.push(value);
        }

        let ctx = Context {
            row: Some(Row {
                schema: &schema,
                values: &values,
            }),
            session,
            subqueries: Some(self),
        };
        expr::eval(&rewritten, &ctx)
    }

    /// Rewrites every aggregate call into a reference to a hidden column, and
    /// appends the folded value for it to `folded`.
    fn fold_aggregates(
        &self,
        e: &Expr,
        group: &[&Vec<Value>],
        refs: &[ColumnRef],
        session: &Session,
        folded: &mut Vec<Value>,
    ) -> EngineResult<Expr> {
        use sqlparser::ast::Ident;

        // A window function is not an aggregate over the group, so it is left
        // for the projection to handle.
        if let Expr::Function(f) = e {
            if f.over.is_none() && is_aggregate(&f.name.to_string()) {
                let name = f.name.to_string().to_ascii_lowercase();
                let argument = aggregate_argument(f);
                let value = self.fold(
                    session,
                    &name,
                    argument.as_ref(),
                    refs,
                    group,
                    aggregate_is_distinct(f),
                )?;
                let slot = folded.len();
                folded.push(value);
                return Ok(Expr::Identifier(Ident::new(folded_name(slot))));
            }
        }

        let recur = |inner: &Expr, folded: &mut Vec<Value>| {
            self.fold_aggregates(inner, group, refs, session, folded)
        };

        Ok(match e {
            Expr::BinaryOp { left, op, right } => Expr::BinaryOp {
                left: Box::new(recur(left, folded)?),
                op: op.clone(),
                right: Box::new(recur(right, folded)?),
            },
            Expr::UnaryOp { op, expr } => Expr::UnaryOp {
                op: *op,
                expr: Box::new(recur(expr, folded)?),
            },
            Expr::Nested(inner) => Expr::Nested(Box::new(recur(inner, folded)?)),
            Expr::IsNull(inner) => Expr::IsNull(Box::new(recur(inner, folded)?)),
            Expr::IsNotNull(inner) => Expr::IsNotNull(Box::new(recur(inner, folded)?)),
            Expr::Between {
                expr,
                negated,
                low,
                high,
            } => Expr::Between {
                expr: Box::new(recur(expr, folded)?),
                negated: *negated,
                low: Box::new(recur(low, folded)?),
                high: Box::new(recur(high, folded)?),
            },
            Expr::Cast {
                kind,
                expr,
                data_type,
                format,
                array,
            } => Expr::Cast {
                kind: kind.clone(),
                expr: Box::new(recur(expr, folded)?),
                data_type: data_type.clone(),
                format: format.clone(),
                array: *array,
            },
            Expr::Case {
                case_token,
                end_token,
                operand,
                conditions,
                else_result,
            } => Expr::Case {
                case_token: case_token.clone(),
                end_token: end_token.clone(),
                operand: match operand {
                    Some(o) => Some(Box::new(recur(o, folded)?)),
                    None => None,
                },
                conditions: {
                    let mut out = Vec::with_capacity(conditions.len());
                    for w in conditions {
                        out.push(sqlparser::ast::CaseWhen {
                            condition: recur(&w.condition, folded)?,
                            result: recur(&w.result, folded)?,
                        });
                    }
                    out
                },
                else_result: match else_result {
                    Some(r) => Some(Box::new(recur(r, folded)?)),
                    None => None,
                },
            },
            Expr::Function(f) => {
                use sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArguments};
                let mut f = f.clone();
                if let FunctionArguments::List(list) = &mut f.args {
                    for arg in &mut list.args {
                        if let FunctionArg::Unnamed(FunctionArgExpr::Expr(inner))
                        | FunctionArg::Named {
                            arg: FunctionArgExpr::Expr(inner),
                            ..
                        } = arg
                        {
                            *inner = recur(inner, folded)?;
                        }
                    }
                }
                Expr::Function(f)
            }
            // Anything without a nested expression to rewrite passes through.
            other => other.clone(),
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
        distinct: bool,
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
                    subqueries: Some(self),
                };
                values.push(expr::eval(arg, &ctx)?);
            }
        }

        // Aggregates ignore NULL, except COUNT(*) which counts rows.
        let mut present: Vec<&Value> = values.iter().filter(|v| !v.is_null()).collect();

        // COUNT(DISTINCT x) and the like fold over unique values only.
        if distinct {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            present.retain(|v| seen.insert(v.collation_key()));
        }

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
                subqueries: Some(self),
            };
            let mut key = Vec::with_capacity(items.len());
            for item in items {
                // `ORDER BY 2` names the second output column. Evaluated as an
                // expression it is the constant 2, which gives every row the
                // same sort key and leaves the result unordered.
                key.push(match ordinal_of(&item.expr).and_then(|n| row.get(n - 1)) {
                    Some(value) => value.clone(),
                    None => expr::eval(&item.expr, &ctx)?,
                });
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

        // Captured before the child table is borrowed mutably, so validation
        // is a pure function of the candidate row.
        let mut fk_parents = {
            let database = catalog
                .database(&db)
                .ok_or_else(|| EngineError::unknown_database(&db))?;
            let table = database
                .table(&table_name)
                .ok_or_else(|| EngineError::no_such_table(&table_name))?;
            foreign_keys::snapshot(database, table)
        };

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
                    let index = table
                        .column_index(&name)
                        .ok_or_else(|| EngineError::unknown_column(&name, "field list"))?;
                    if table.columns[index].generated.is_some() {
                        return Err(EngineError::new(
                            3105,
                            "HY000",
                            format!(
                                "The value specified for generated column '{name}' in table \
                                 '{}' is not allowed.",
                                table.name
                            ),
                        ));
                    }
                    Ok(index)
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

            let ctx = Context::rowless(&session_snapshot).with_subqueries(self);
            let mut row = default_row(table, &ctx)?;

            for (slot, value_expr) in targets.iter().zip(values.content.iter()) {
                if table.columns[*slot].generated.is_some() {
                    // DEFAULT and NULL are the only values MySQL accepts in a
                    // generated column's position.
                    let permitted = matches!(value_expr, Expr::Identifier(i)
                        if i.value.eq_ignore_ascii_case("default"))
                        || matches!(value_expr, Expr::Value(v)
                            if matches!(v.value, sqlparser::ast::Value::Null));
                    if !permitted {
                        return Err(EngineError::new(
                            3105,
                            "HY000",
                            format!(
                                "The value specified for generated column '{}' in table '{}' \
                                 is not allowed.",
                                table.columns[*slot].name, table.name
                            ),
                        ));
                    }
                    continue;
                }
                // DEFAULT in a VALUES list keeps the column's default.
                if matches!(value_expr, Expr::Identifier(i) if i.value.eq_ignore_ascii_case("default"))
                {
                    continue;
                }
                let value = expr::eval(value_expr, &ctx)?;
                let column = &table.columns[*slot];
                row[*slot] = column.ty.coerce(value, &column.name)?;
            }

            // The counter lives on the table, so the column list and the row
            // are walked together rather than by index.
            let auto_increment: Vec<usize> = table
                .columns
                .iter()
                .enumerate()
                .filter(|(_, c)| c.auto_increment)
                .map(|(i, _)| i)
                .collect();
            for slot in auto_increment {
                if row[slot].is_null() {
                    row[slot] = Value::Int(table.next_auto_increment);
                    last_id = table.next_auto_increment as u64;
                    table.next_auto_increment += 1;
                } else if let Some(explicit) = row[slot].as_int() {
                    // An explicit value moves the counter past it, as MySQL
                    // does. Without this, a fixture that inserts explicit ids
                    // collides with generated ones later.
                    table.next_auto_increment = table.next_auto_increment.max(explicit + 1);
                }
            }

            // Generated columns, NOT NULL and CHECK are all validated here, on
            // the same path UPDATE uses.
            let row = self.finalise_row(table, row, &session_snapshot)?;

            // IGNORE covers a foreign key violation as well as a duplicate
            // key; MySQL downgrades both to a warning and skips the row.
            let admissible = foreign_keys::check(&fk_parents, &row, &table_name)
                .and_then(|()| table.check_unique(&row, None));
            match admissible {
                Ok(()) => {}
                Err(_) if ignore => continue,
                Err(e) => return Err(e),
            }

            // A self-referencing constraint must see this row when the next
            // one in the same statement is checked.
            for parent in &mut fk_parents {
                parent.note_inserted(&row);
            }

            table.rows.push(row);
            inserted += 1;
        }

        mark_modified(session, &db, std::slice::from_ref(&table_name));
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

        let fk_parents = {
            let database = catalog
                .database(&db)
                .ok_or_else(|| EngineError::unknown_database(&db))?;
            let table = database
                .table(&table_name)
                .ok_or_else(|| EngineError::no_such_table(&table_name))?;
            foreign_keys::snapshot(database, table)
        };

        let database = catalog
            .database_mut(&db)
            .ok_or_else(|| EngineError::unknown_database(&db))?;

        // Applied to a copy and swapped in only on success, so a RESTRICT
        // found while cascading to children cannot leave the parent updated.
        let mut working = database.clone();
        let table = working
            .table_mut(&table_name)
            .ok_or_else(|| EngineError::no_such_table(&table_name))?;

        let refs: Vec<ColumnRef> = table
            .columns
            .iter()
            .map(|c| ColumnRef::new(&table.name, &c.name))
            .collect();

        // Checked once, before any row is considered: whether writing to a
        // generated column is an error must not depend on the data.
        for assignment in assignments {
            let column_name = assignment_column(&assignment.target)?;
            let slot = table
                .column_index(&column_name)
                .ok_or_else(|| EngineError::unknown_column(&column_name, "field list"))?;
            if table.columns[slot].generated.is_some() {
                return Err(EngineError::new(
                    3105,
                    "HY000",
                    format!(
                        "The value specified for generated column '{column_name}' in table \
                         '{}' is not allowed.",
                        table.name
                    ),
                ));
            }
        }

        let mut updated = 0u64;
        let mut changed: Vec<(Vec<Value>, Vec<Value>)> = Vec::new();
        let mut rows = table.rows.clone();

        for index in 0..rows.len() {
            let ctx = Context {
                row: Some(Row {
                    schema: &refs,
                    values: &rows[index],
                }),
                session: &session_snapshot,
                subqueries: Some(self),
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
                candidate[slot] = column.ty.coerce(value, &column.name)?;
            }

            let candidate = self.finalise_row(table, candidate, &session_snapshot)?;
            foreign_keys::check(&fk_parents, &candidate, &table_name)?;
            table.check_unique_against(&rows, &candidate, index)?;
            // Children follow the key, so both halves of the change are needed.
            changed.push((rows[index].clone(), candidate.clone()));
            rows[index] = candidate;
            updated += 1;
        }

        table.rows = rows;
        let mut touched = vec![table_name.clone()];
        cascade_update(&mut working, &table_name, &changed, &mut touched)?;
        *database = working;
        mark_modified(session, &db, &touched);

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

        // The delete and its cascade are applied to a copy and swapped in only
        // if the whole thing succeeds. A RESTRICT discovered part-way through a
        // chain would otherwise leave the parent already removed and earlier
        // children already cascaded.
        let mut working = database.clone();
        let table = working
            .table_mut(&table_name)
            .ok_or_else(|| EngineError::no_such_table(&table_name))?;

        let refs: Vec<ColumnRef> = table
            .columns
            .iter()
            .map(|c| ColumnRef::new(&table.name, &c.name))
            .collect();

        let before = table.rows.len();
        let mut kept = Vec::with_capacity(before);
        let mut removed_rows: Vec<Vec<Value>> = Vec::new();
        for row in std::mem::take(&mut table.rows) {
            let ctx = Context {
                row: Some(Row {
                    schema: &refs,
                    values: &row,
                }),
                session: &session_snapshot,
                subqueries: Some(self),
            };
            let matched = match &delete.selection {
                Some(p) => expr::eval(p, &ctx)?.truthy() == Some(true),
                None => true,
            };
            if matched {
                removed_rows.push(row);
            } else {
                kept.push(row);
            }
        }

        let removed = removed_rows.len() as u64;
        table.rows = kept;

        // Children are handled after the parent rows are removed, so a
        // self-referencing table does not see the rows it is cascading from.
        let mut touched = vec![table_name.clone()];
        cascade_delete(&mut working, &table_name, &removed_rows, &mut touched)?;
        *database = working;
        mark_modified(session, &db, &touched);

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
            ObjectType::View => {
                for name in names {
                    let (db, view) = self.split_name(session, name)?;
                    let mut catalog = self.catalog_mut()?;
                    let Some(database) = catalog.database_mut(&db) else {
                        if if_exists {
                            continue;
                        }
                        return Err(EngineError::unknown_database(&db));
                    };
                    let key = database
                        .views
                        .keys()
                        .find(|k| k.eq_ignore_ascii_case(&view))
                        .cloned();
                    match key {
                        Some(k) => {
                            database.views.remove(&k);
                        }
                        None if if_exists => {}
                        None => return Err(EngineError::no_such_table(&view)),
                    }
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
                let ctx = Context::rowless(session).with_subqueries(self);
                // An expression that cannot be evaluated must not become a
                // silent NULL: the variable would then carry a wrong answer
                // into whatever reads it next.
                let value = match values.first() {
                    Some(e) => expr::eval(e, &ctx)?,
                    None => Value::Null,
                };
                self.assign(session, &name, value);
                Ok(Outcome::ok())
            }
            S::MultipleAssignments { assignments } => {
                for assignment in assignments {
                    let name = object_name(&assignment.name);
                    let ctx = Context::rowless(session).with_subqueries(self);
                    let value = expr::eval(&assignment.value, &ctx)?;
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
        // MySQL lists views here too, and information_schema.TABLES already
        // reports them, so omitting them contradicts the same connection.
        let mut names: Vec<&String> = database
            .tables
            .keys()
            .chain(database.views.keys())
            .collect();
        names.sort();
        for name in names {
            result.rows.push(vec![Value::Text(name.clone())]);
        }
        Ok(result)
    }

    fn show_columns(&self, session: &Session, table: &str) -> EngineResult<ResultSet> {
        let catalog = self.catalog()?;
        let database = catalog.resolve(None, session.current_database.as_deref())?;
        let mut result = ResultSet::new(vec![
            OutColumn::new("Field", ColumnType::VarString),
            OutColumn::new("Type", ColumnType::VarString),
            OutColumn::new("Null", ColumnType::VarString),
            OutColumn::new("Key", ColumnType::VarString),
            OutColumn::new("Default", ColumnType::VarString),
            OutColumn::new("Extra", ColumnType::VarString),
        ]);

        // A view is described by the shape resolved when it was created, which
        // is also what information_schema reports for it.
        if let Some(view) = database.view(table) {
            for column in &view.shape {
                result.rows.push(vec![
                    Value::Text(column.name.clone()),
                    Value::Text(column.ty.declared.clone()),
                    Value::text(if column.nullable { "YES" } else { "NO" }),
                    Value::text(""),
                    Value::Null,
                    Value::text(""),
                ]);
            }
            return Ok(result);
        }

        let table = database
            .table(table)
            .ok_or_else(|| EngineError::no_such_table(table))?;

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

    /// Runs a view's definition and presents the result as a relation.
    ///
    /// The view's own database is made current for the duration, so an
    /// unqualified table reference inside the definition resolves where the
    /// view lives rather than wherever the caller happens to be.
    fn materialise_view(
        &self,
        session: &Session,
        view: &crate::catalog::View,
        database: &str,
        alias: &str,
    ) -> EngineResult<Relation> {
        let mut scoped = session.clone();
        scoped.current_database = Some(database.to_owned());
        // Views nest; without a guard a view defined in terms of itself would
        // recurse until the stack ran out.
        scoped.view_depth += 1;
        if scoped.view_depth > 16 {
            return Err(EngineError::new(
                1462,
                "HY000",
                format!("`{}` contains a recursive view reference", view.name),
            ));
        }

        let outcome = self.execute(&mut scoped, &view.definition, &[])?;
        let Outcome::Rows(rows) = outcome else {
            return Err(EngineError::new(
                1064,
                "42000",
                format!("View `{}` does not return rows", view.name),
            ));
        };

        // Explicit column names from `CREATE VIEW v (a, b)` rename the output.
        let columns = rows
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| crate::relation::RelColumn {
                schema: database.to_owned(),
                table: alias.to_owned(),
                name: view
                    .columns
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| c.name.clone()),
                // The inner query already worked out each column's type;
                // discarding it reports every view column as text, and a typed
                // client then decodes a number as a string.
                ty: crate::types::SqlType::from_column_type(c.ty),
                nullable: c.nullable,
            })
            .collect();

        Ok(Relation {
            columns,
            rows: rows.rows,
        })
    }

    /// Computes generated columns and validates a candidate row.
    ///
    /// Shared by INSERT and UPDATE: a constraint enforced on one path and not
    /// the other is worse than one enforced on neither, because the gap is
    /// invisible.
    fn finalise_row(
        &self,
        table: &Table,
        mut row: Vec<Value>,
        session: &Session,
    ) -> EngineResult<Vec<Value>> {
        let refs: Vec<ColumnRef> = table
            .columns
            .iter()
            .map(|c| ColumnRef::new(&table.name, &c.name))
            .collect();

        // Generated columns are computed from the row's other values, so they
        // are evaluated once everything else is in place.
        for index in 0..table.columns.len() {
            let Some(expression) = table.columns[index].generated.clone() else {
                continue;
            };
            let parsed = parse_expression(&expression)?;
            let value = {
                let ctx = Context {
                    row: Some(Row {
                        schema: &refs,
                        values: &row,
                    }),
                    session,
                    subqueries: Some(self),
                };
                expr::eval(&parsed, &ctx)?
            };
            let column = &table.columns[index];
            row[index] = column.ty.coerce(value, &column.name)?;
        }

        for (index, column) in table.columns.iter().enumerate() {
            if !column.nullable && row[index].is_null() {
                return Err(EngineError::null_not_allowed(&column.name));
            }
        }

        for check in &table.checks {
            let parsed = parse_expression(&check.expr)?;
            let ctx = Context {
                row: Some(Row {
                    schema: &refs,
                    values: &row,
                }),
                session,
                subqueries: Some(self),
            };
            // A CHECK passes when it is true or unknown; only an outright
            // false is a violation, as in MySQL.
            if expr::eval(&parsed, &ctx)?.truthy() == Some(false) {
                return Err(EngineError::new(
                    3819,
                    "HY000",
                    format!("Check constraint '{}' is violated.", check.name),
                ));
            }
        }

        Ok(row)
    }

    /// Splits a rendered `db.table` string, for the paths where the AST hands
    /// back a name already stringified.
    fn split_parts(&self, session: &Session, rendered: &str) -> EngineResult<(String, String)> {
        let parts: Vec<String> = rendered
            .split('.')
            // Under ANSI_QUOTES the parts arrive double-quoted, so trimming
            // only backticks leaves a database named `testdb"`.
            .map(|p| p.trim_matches(|c| c == '`' || c == '"').to_owned())
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

impl crate::expr::Subqueries for Engine {
    fn rows(
        &self,
        query: &sqlparser::ast::Query,
        session: &Session,
        outer: Option<crate::expr::OuterRow>,
    ) -> EngineResult<Vec<Vec<Value>>> {
        Ok(self.query_with_outer(session, query, outer.as_ref())?.rows)
    }
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
    /// Precomputed per row by a window function.
    Window(usize),
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

/// Records tables a statement wrote, so a rollback knows what to undo.
///
/// Only meaningful inside a transaction; outside one there is nothing to undo.
fn mark_modified(session: &mut Session, database: &str, tables: &[String]) {
    let Some(transaction) = session.transaction.as_mut() else {
        return;
    };
    for table in tables {
        let pair = (database.to_owned(), table.clone());
        if !transaction.modified.contains(&pair) {
            transaction.modified.push(pair);
        }
    }
}

/// Restores the named tables from a snapshot, leaving everything else alone.
///
/// Only the tables a transaction wrote are rolled back. Replacing the whole
/// catalog would also discard work another connection had already committed,
/// because the catalog is shared between connections.
fn restore_tables(catalog: &mut Catalog, snapshot: &Catalog, tables: &[(String, String)]) {
    for (database_name, table_name) in tables {
        let original = snapshot
            .database(database_name)
            .and_then(|d| d.table(table_name))
            .cloned();
        let Some(database) = catalog.database_mut(database_name) else {
            continue;
        };

        let key = database
            .tables
            .keys()
            .find(|k| k.eq_ignore_ascii_case(table_name))
            .cloned();
        match original {
            // The table existed before; put it back as it was.
            Some(table) => {
                let key = key.unwrap_or_else(|| table.name.clone());
                database.tables.insert(key, table);
            }
            // It did not, so the transaction created it.
            None => {
                if let Some(key) = key {
                    database.tables.remove(&key);
                }
            }
        }
    }
}

/// Applies each dependent table's ON UPDATE action after a parent key changes.
///
/// Without this an UPDATE of a parent key leaves children pointing at a value
/// that no longer exists — a dangling reference the engine would then reject on
/// the next INSERT of the same value.
fn cascade_update(
    database: &mut Database,
    parent_table: &str,
    changed: &[(Vec<Value>, Vec<Value>)],
    touched: &mut Vec<String>,
) -> EngineResult<()> {
    if changed.is_empty() {
        return Ok(());
    }
    let Some(parent) = database.table(parent_table).cloned() else {
        return Ok(());
    };

    for dependent in foreign_keys::dependents(database, &parent) {
        // Only a change to the referenced columns matters; updating an
        // unrelated column leaves children alone.
        let moves: Vec<(Vec<String>, Vec<Value>)> = changed
            .iter()
            .filter_map(|(before, after)| {
                let old: Vec<String> = dependent
                    .parent_positions
                    .iter()
                    .map(|p| {
                        before
                            .get(*p)
                            .cloned()
                            .unwrap_or(Value::Null)
                            .collation_key()
                    })
                    .collect();
                let new: Vec<Value> = dependent
                    .parent_positions
                    .iter()
                    .map(|p| after.get(*p).cloned().unwrap_or(Value::Null))
                    .collect();
                let unchanged = new.iter().map(Value::collation_key).eq(old.iter().cloned());
                (!unchanged).then_some((old, new))
            })
            .collect();

        if moves.is_empty() {
            continue;
        }

        let action = Action::parse(&dependent.constraint.on_update);
        let Some(child) = database.table_mut(&dependent.table) else {
            continue;
        };
        touched.push(dependent.table.clone());

        for row in &mut child.rows {
            if dependent
                .child_positions
                .iter()
                .any(|p| row.get(*p).is_some_and(Value::is_null))
            {
                continue;
            }
            let key: Vec<String> = dependent
                .child_positions
                .iter()
                .map(|p| row.get(*p).cloned().unwrap_or(Value::Null).collation_key())
                .collect();
            let Some((_, new)) = moves.iter().find(|(old, _)| *old == key) else {
                continue;
            };

            match action {
                Action::Restrict => {
                    return Err(foreign_keys::restrict_violation(
                        &dependent.table,
                        parent_table,
                        &dependent.constraint,
                    ))
                }
                Action::Cascade => {
                    for (slot, value) in dependent.child_positions.iter().zip(new) {
                        row[*slot] = value.clone();
                    }
                }
                Action::SetNull => {
                    for slot in &dependent.child_positions {
                        row[*slot] = Value::Null;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Applies each dependent table's ON DELETE action after parent rows are gone.
///
/// Cascades chain — deleting a job removes its timesheets, which removes their
/// records — so this works one generation at a time rather than one level. The
/// cap counts generations, not tables: a hub with twenty referencing tables is
/// one level deep, not twenty.
fn cascade_delete(
    database: &mut Database,
    parent_table: &str,
    removed: &[Vec<Value>],
    touched: &mut Vec<String>,
) -> EngineResult<()> {
    const MAX_GENERATIONS: usize = 16;

    let mut generation: Vec<(String, Vec<Vec<Value>>)> =
        vec![(parent_table.to_owned(), removed.to_vec())];

    for _ in 0..MAX_GENERATIONS {
        let mut next: Vec<(String, Vec<Vec<Value>>)> = Vec::new();

        for (parent_name, parent_rows) in generation {
            if parent_rows.is_empty() {
                continue;
            }
            let Some(parent) = database.table(&parent_name).cloned() else {
                continue;
            };

            for dependent in foreign_keys::dependents(database, &parent) {
                // The set of parent keys that just disappeared.
                let gone: std::collections::HashSet<Vec<String>> = parent_rows
                    .iter()
                    .map(|row| {
                        dependent
                            .parent_positions
                            .iter()
                            .map(|p| row.get(*p).cloned().unwrap_or(Value::Null).collation_key())
                            .collect()
                    })
                    .collect();

                let action = Action::parse(&dependent.constraint.on_delete);

                // SET NULL cannot write into a NOT NULL column. MySQL rejects
                // that DDL up front; here the write is refused rather than
                // storing a row the schema says is impossible.
                if action == Action::SetNull {
                    let child = database.table(&dependent.table);
                    if let Some(child) = child {
                        if let Some(column) = dependent
                            .child_positions
                            .iter()
                            .filter_map(|p| child.columns.get(*p))
                            .find(|c| !c.nullable)
                        {
                            return Err(EngineError::new(
                                1830,
                                "HY000",
                                format!(
                                    "Column '{}' cannot be NOT NULL: needed in a foreign key \
                                     constraint '{}' SET NULL",
                                    column.name, dependent.constraint.name
                                ),
                            ));
                        }
                    }
                }

                let Some(child) = database.table_mut(&dependent.table) else {
                    continue;
                };
                touched.push(dependent.table.clone());

                let matches = |row: &Vec<Value>| -> bool {
                    // A NULL reference points at nothing, so it is unaffected.
                    if dependent
                        .child_positions
                        .iter()
                        .any(|p| row.get(*p).is_some_and(Value::is_null))
                    {
                        return false;
                    }
                    let key: Vec<String> = dependent
                        .child_positions
                        .iter()
                        .map(|p| row.get(*p).cloned().unwrap_or(Value::Null).collation_key())
                        .collect();
                    gone.contains(&key)
                };

                match action {
                    Action::Restrict => {
                        if child.rows.iter().any(matches) {
                            return Err(foreign_keys::restrict_violation(
                                &dependent.table,
                                &parent_name,
                                &dependent.constraint,
                            ));
                        }
                    }
                    Action::SetNull => {
                        for row in &mut child.rows {
                            if matches(row) {
                                for position in &dependent.child_positions {
                                    row[*position] = Value::Null;
                                }
                            }
                        }
                    }
                    Action::Cascade => {
                        let mut removed_here = Vec::new();
                        let mut kept = Vec::with_capacity(child.rows.len());
                        for row in std::mem::take(&mut child.rows) {
                            if matches(&row) {
                                removed_here.push(row);
                            } else {
                                kept.push(row);
                            }
                        }
                        child.rows = kept;
                        // Whatever referenced these rows needs the same
                        // treatment in the next generation.
                        if !removed_here.is_empty() {
                            next.push((dependent.table.clone(), removed_here));
                        }
                    }
                }
            }
        }

        if next.is_empty() {
            return Ok(());
        }
        generation = next;
    }

    Err(EngineError::new(
        1456,
        "HY000",
        "Cascading foreign key actions nest too deeply",
    ))
}

/// Parses a stored expression, such as a column default or a CHECK body.
fn parse_expression(text: &str) -> EngineResult<Expr> {
    Parser::new(&MySqlDialect {})
        .try_with_sql(text)
        .and_then(|mut p| p.parse_expr())
        .map_err(|e| EngineError::parse(format!("{e}")))
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
    // LIMIT and OFFSET are constants or parameters, never subqueries.
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

/// Removes duplicate rows in place, preserving first-seen order.
///
/// The key uses the same collation as comparison, so what DISTINCT and UNION
/// treat as duplicate matches what `WHERE` treats as equal.
fn dedupe(rows: &mut Vec<Vec<Value>>) {
    // A set, not a Vec: UNION dedupes the concatenation of both branches by
    // default, and a linear probe there is quadratic in the row count.
    let mut seen: std::collections::HashSet<Vec<String>> = std::collections::HashSet::new();
    let mut kept = Vec::with_capacity(rows.len());
    for row in std::mem::take(rows) {
        // NULL is its own distinct value and must not collide with the empty
        // string.
        let key: Vec<String> = row
            .iter()
            .map(|v| {
                if v.is_null() {
                    "\u{0}NULL".to_owned()
                } else {
                    v.collation_key()
                }
            })
            .collect();
        if seen.insert(key) {
            kept.push(row);
        }
    }
    *rows = kept;
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
            dedupe(&mut result.rows);
            Ok(())
        }
        Some(other) => Err(unsupported(format!("`{other}`"))),
    }
}

/// The name of the hidden column a folded aggregate is bound to.
///
/// A NUL prefix keeps it out of any namespace a query could reference.
fn folded_name(index: usize) -> String {
    format!("\u{0}agg{index}")
}

/// The alias and expression of each projected item.
fn projected_items(projection: &[SelectItem]) -> EngineResult<Vec<(String, Expr)>> {
    projection
        .iter()
        .map(|item| match item {
            SelectItem::UnnamedExpr(e) => Ok((render_expr(e), e.clone())),
            SelectItem::ExprWithAlias { expr, alias } => Ok((alias.value.clone(), expr.clone())),
            other => Err(unsupported(format!(
                "select item `{other}` in a grouped query"
            ))),
        })
        .collect()
}

/// The 1-based position an expression names, if it is a bare integer.
fn ordinal_of(e: &Expr) -> Option<usize> {
    match e {
        Expr::Value(v) => match &v.value {
            sqlparser::ast::Value::Number(n, _) => n.parse::<usize>().ok().filter(|n| *n > 0),
            _ => None,
        },
        _ => None,
    }
}

/// Resolves `GROUP BY 1` to the first select item's expression.
fn resolve_ordinal(e: &Expr, projection: &[SelectItem]) -> Option<Expr> {
    let position = ordinal_of(e)?;
    match projection.get(position - 1)? {
        SelectItem::UnnamedExpr(inner) => Some(inner.clone()),
        SelectItem::ExprWithAlias { expr, .. } => Some(expr.clone()),
        _ => None,
    }
}

/// Replaces references to a select-list alias with the value already computed
/// for it, so HAVING and ORDER BY can name one.
fn substitute_aliases(e: &Expr, items: &[(String, Expr)], values: &[Value]) -> Expr {
    if let Expr::Identifier(ident) = e {
        if let Some(position) = items
            .iter()
            .position(|(alias, _)| alias.eq_ignore_ascii_case(&ident.value))
        {
            if let Some(value) = values.get(position) {
                return literal_expr(value);
            }
        }
    }

    let recur = |inner: &Expr| Box::new(substitute_aliases(inner, items, values));
    match e {
        Expr::BinaryOp { left, op, right } => Expr::BinaryOp {
            left: recur(left),
            op: op.clone(),
            right: recur(right),
        },
        Expr::UnaryOp { op, expr } => Expr::UnaryOp {
            op: *op,
            expr: recur(expr),
        },
        Expr::Nested(inner) => Expr::Nested(recur(inner)),
        Expr::IsNull(inner) => Expr::IsNull(recur(inner)),
        Expr::IsNotNull(inner) => Expr::IsNotNull(recur(inner)),
        Expr::Between {
            expr,
            negated,
            low,
            high,
        } => Expr::Between {
            expr: recur(expr),
            negated: *negated,
            low: recur(low),
            high: recur(high),
        },
        other => other.clone(),
    }
}

/// Renders a value as a literal expression.
///
/// Only used for alias substitution, where the values involved are the ones a
/// comparison in HAVING or ORDER BY will act on.
fn literal_expr(value: &Value) -> Expr {
    use sqlparser::ast::{Value as AstValue, ValueWithSpan};
    let span = sqlparser::tokenizer::Span::empty();
    let wrap = |v: AstValue| Expr::Value(ValueWithSpan { value: v, span });

    match value {
        Value::Null => wrap(AstValue::Null),
        Value::Int(v) => wrap(AstValue::Number(v.to_string(), false)),
        Value::Float(v) if v.is_finite() => wrap(AstValue::Number(format!("{v:?}"), false)),
        Value::Float(_) => wrap(AstValue::Null),
        Value::Text(v) => wrap(AstValue::SingleQuotedString(v.clone())),
        Value::Bytes(v) => wrap(AstValue::SingleQuotedString(
            String::from_utf8_lossy(v).into_owned(),
        )),
    }
}

/// The declared type of a computed column.
///
/// A grouped projection cannot be typed from the first group's value: a later
/// group may produce a different type, and the binary protocol encodes by the
/// declared type, so the client would decode the wrong thing. Only the shapes
/// that are always integral are claimed as such.
fn computed_column_type(e: &Expr) -> ColumnType {
    match e {
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_lowercase();
            match name.as_str() {
                "count" | "row_number" | "rank" | "dense_rank" => ColumnType::LongLong,
                _ => ColumnType::VarString,
            }
        }
        _ => ColumnType::VarString,
    }
}

/// Rejects a selected expression that is neither aggregated nor grouped by.
///
/// Without this the engine evaluates it against the group's first row and
/// returns an arbitrary value, where MySQL under ONLY_FULL_GROUP_BY errors.
fn check_full_group_by(e: &Expr, group_exprs: &[Expr]) -> EngineResult<()> {
    // An expression built only from aggregates and constants is always fine.
    if !references_column(e) {
        return Ok(());
    }
    // So is one the query grouped by, compared as written.
    let rendered = e.to_string();
    if group_exprs.iter().any(|g| g.to_string() == rendered) {
        return Ok(());
    }

    match e {
        Expr::Function(f) if f.over.is_some() => return Ok(()),
        Expr::Function(f) if is_aggregate(&f.name.to_string()) => return Ok(()),
        Expr::BinaryOp { left, right, .. } => {
            check_full_group_by(left, group_exprs)?;
            return check_full_group_by(right, group_exprs);
        }
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) | Expr::Cast { expr, .. } => {
            return check_full_group_by(expr, group_exprs)
        }
        _ => {}
    }

    Err(EngineError::new(
        1055,
        "42000",
        format!(
            "Expression '{rendered}' is not in GROUP BY clause and contains a nonaggregated \
             column; this is incompatible with sql_mode=only_full_group_by"
        ),
    ))
}

/// Whether an expression reads any column outside an aggregate.
fn references_column(e: &Expr) -> bool {
    match e {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        // An aggregate consumes its columns, so they are not loose references.
        Expr::Function(f) if f.over.is_none() && is_aggregate(&f.name.to_string()) => false,
        Expr::Function(f) => function_arguments(f).iter().any(references_column),
        Expr::BinaryOp { left, right, .. } => references_column(left) || references_column(right),
        Expr::UnaryOp { expr, .. }
        | Expr::Nested(expr)
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::Cast { expr, .. } => references_column(expr),
        Expr::Between {
            expr, low, high, ..
        } => references_column(expr) || references_column(low) || references_column(high),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            operand.as_deref().is_some_and(references_column)
                || conditions
                    .iter()
                    .any(|w| references_column(&w.condition) || references_column(&w.result))
                || else_result.as_deref().is_some_and(references_column)
        }
        _ => false,
    }
}

/// True when a select item contains an aggregate anywhere inside it.
///
/// Matching only a bare `COUNT(*)` misses `SUM(a) / COUNT(*)` and
/// `IFNULL(SUM(v), 0)`, which then take the per-row path and either fail
/// outright or return no rows at all.
fn select_item_aggregates(item: &SelectItem) -> bool {
    match item {
        SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
            contains_aggregate(e)
        }
        _ => false,
    }
}

/// True when an expression contains an aggregate call anywhere inside it.
fn contains_aggregate(e: &Expr) -> bool {
    match e {
        Expr::Function(f) => {
            f.over.is_none() && is_aggregate(&f.name.to_string())
                || function_arguments(f).iter().any(contains_aggregate)
        }
        Expr::BinaryOp { left, right, .. } => contains_aggregate(left) || contains_aggregate(right),
        Expr::UnaryOp { expr, .. } => contains_aggregate(expr),
        Expr::Nested(inner) | Expr::IsNull(inner) | Expr::IsNotNull(inner) => {
            contains_aggregate(inner)
        }
        Expr::Cast { expr, .. } => contains_aggregate(expr),
        Expr::Between {
            expr, low, high, ..
        } => contains_aggregate(expr) || contains_aggregate(low) || contains_aggregate(high),
        Expr::InList { expr, list, .. } => {
            contains_aggregate(expr) || list.iter().any(contains_aggregate)
        }
        Expr::InSubquery { expr, .. } => contains_aggregate(expr),
        Expr::Like { expr, pattern, .. } => contains_aggregate(expr) || contains_aggregate(pattern),
        Expr::IsTrue(inner)
        | Expr::IsFalse(inner)
        | Expr::IsNotTrue(inner)
        | Expr::IsNotFalse(inner) => contains_aggregate(inner),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            operand.as_deref().is_some_and(contains_aggregate)
                || conditions
                    .iter()
                    .any(|w| contains_aggregate(&w.condition) || contains_aggregate(&w.result))
                || else_result.as_deref().is_some_and(contains_aggregate)
        }
        _ => false,
    }
}

/// The expression arguments of a function call.
fn function_arguments(f: &sqlparser::ast::Function) -> Vec<Expr> {
    use sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArguments};
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
            } => Some(e.clone()),
            _ => None,
        })
        .collect()
}

/// Whether an aggregate was written with DISTINCT.
fn aggregate_is_distinct(f: &sqlparser::ast::Function) -> bool {
    use sqlparser::ast::{DuplicateTreatment, FunctionArguments};
    matches!(
        &f.args,
        FunctionArguments::List(list) if list.duplicate_treatment == Some(DuplicateTreatment::Distinct)
    )
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
