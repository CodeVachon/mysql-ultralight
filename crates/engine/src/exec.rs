//! Statement execution.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use mysql_ultralight_protocol::ColumnType;
use sqlparser::ast::Spanned;
use sqlparser::ast::{
    Assignment, AssignmentTarget, Expr, GroupByExpr, Insert, JoinOperator, ObjectName, ObjectType,
    OrderBy, OrderByExpr, OrderByKind, Query, Select, SelectItem, SetExpr, Statement, TableFactor,
    TableWithJoins,
};
use sqlparser::dialect::MySqlDialect;
use sqlparser::parser::Parser;

use crate::catalog::{Catalog, Database, Table, INFORMATION_SCHEMA};
use crate::ddl;
use crate::error::{unsupported, EngineError, EngineResult};
use crate::expr::{self, ColumnRef, Context, OuterRow};
use crate::foreign_keys::{self, Action};
use crate::image;
use crate::info_schema;
use crate::name;
use crate::relation::Relation;
use crate::result::{OutColumn, Outcome, ResultSet};
use crate::rewrite::{self, Intercepted, ShowDiagnostics};
use crate::session::{ServerConfig, Session};
use crate::value::Value;

/// The database server's shared state.
#[derive(Debug)]
pub struct Engine {
    catalog: RwLock<Catalog>,
    config: ServerConfig,
    /// Named advisory locks, by lock name to the connection holding it.
    /// Separate from the catalog because they survive a ROLLBACK and are not
    /// part of any database.
    locks: std::sync::Mutex<HashMap<String, u32>>,
    /// Counter behind `UUID()`. Deterministic on purpose: a test that asserts
    /// on a generated id should see the same one every run.
    uuids: std::sync::atomic::AtomicU64,
    /// State behind `RAND()`, deterministic for the same reason.
    randoms: std::sync::atomic::AtomicU64,
    /// Held by one writer at a time, across its copy-compute-commit cycle.
    ///
    /// Writers cannot hold the catalog's write lock while they compute — a
    /// subquery would deadlock on it — so this is what stops two of them
    /// computing against the same copy and each discarding the other's work.
    writes: std::sync::Mutex<()>,
}

impl Engine {
    pub fn new(config: ServerConfig) -> Arc<Self> {
        Arc::new(Self {
            locks: std::sync::Mutex::new(HashMap::new()),
            uuids: std::sync::atomic::AtomicU64::new(1),
            randoms: std::sync::atomic::AtomicU64::new(0x2545_f491_4f6c_dd1d),
            writes: std::sync::Mutex::new(()),
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

    /// The columns a statement will return, worked out before it runs.
    ///
    /// A client caches the metadata from `COM_STMT_PREPARE` and reads every
    /// execution against it, so declaring nothing there costs the result its
    /// column *names*: the client falls back to positional ones and any lookup
    /// by name silently finds nothing. Prisma's migration engine does exactly
    /// that lookup, and concluded `_prisma_migrations` did not exist in a
    /// result that plainly contained it.
    ///
    /// Only a read can be probed, and it is run with every parameter bound to
    /// NULL, because a statement's shape does not depend on its arguments.
    /// `Ok(None)` means the shape could not be established without running
    /// something that might write — the caller then declares no columns, which
    /// is where this started but is at least not a wrong answer.
    ///
    /// `Err` is the other thing MySQL does at prepare time: refuse. A name
    /// that resolves against nothing is refused there before the statement is
    /// ever executed, and a client that prepares its statements up front —
    /// which is what the binary protocol is for — should learn about it then
    /// rather than one round trip later. Which refusals qualify is
    /// `refused_at_prepare`'s decision, because most of what a probe turns
    /// down it turns down for being a probe.
    pub fn describe(
        &self,
        session: &Session,
        sql: &str,
        parameters: usize,
    ) -> EngineResult<Option<Vec<OutColumn>>> {
        let _memo = SubqueryMemo::fresh();
        let bound = rewrite::bind(sql, &vec![Value::Null; parameters]);
        match rewrite::intercept(&bound.sql) {
            // 8.3 refuses the diagnostics statements in the prepared-statement
            // protocol outright, measured: `COM_STMT_PREPARE` of `SHOW
            // WARNINGS` is 1295 there, where every other statement intercepted
            // here prepares. Answering one would be this engine doing
            // something a real server will not -- a suite that prepares `SHOW
            // WARNINGS` would pass here and fail against MySQL, which is the
            // shape of gap this engine exists to close rather than to open.
            Some(Intercepted::ShowDiagnostics(_)) => {
                return Err(EngineError::new(
                    1295,
                    "HY000",
                    "This command is not supported in the prepared statement protocol yet",
                ))
            }
            Some(_) => return Ok(None),
            None => {}
        }

        let normalised = std::rc::Rc::new(rewrite::normalise(bound, session.sql_mode()));
        // Held for the whole call: a computed column is named after the text
        // it was written as, and that text is only available here.
        let _source = Source::set(&normalised, session);
        let Ok(statements) = Parser::parse_sql(&MySqlDialect {}, &normalised.sql) else {
            return Ok(None);
        };
        let [Statement::Query(query)] = statements.as_slice() else {
            return Ok(None);
        };
        // `WITH ... UPDATE` is a query whose body writes, and probing it would
        // apply the statement twice.
        if matches!(
            query.body.as_ref(),
            SetExpr::Update(_) | SetExpr::Insert(_) | SetExpr::Delete(_)
        ) {
            return Ok(None);
        }

        // Run as a probe over no rows: a SELECT is not automatically free of
        // consequences, and the functions that are not are refused where they
        // are called rather than by reading the statement, which would miss
        // the ones inside a view's definition.
        //
        // Shaping is what keeps this cheap. Every column type the projection
        // declares is derived from the expression and the input schema rather
        // than from a value, so the same code answers the same way over an
        // empty table — and preparing a query stops costing as much as running
        // it. `describe_matches_execute` holds the two to each other.
        match expr::probing(|| shaping(|| self.query(session, query))) {
            Ok(result) => Ok(Some(result.columns)),
            Err(e) if refused_at_prepare(&e) => Err(e),
            Err(_) => Ok(None),
        }
    }

    /// Re-resolves every view in a database against the schema as it now is.
    ///
    /// Only `ALTER TABLE` needs this: it is the one statement that can change
    /// a column's type while leaving the views over it valid. Dropping the
    /// table or the column makes them fail on use instead, which is visible
    /// rather than silent.
    ///
    /// Best effort by design — a view this no longer resolves keeps the shape
    /// it had, because an `ALTER` must not fail on account of a view it did
    /// not touch. Resolving reads the schema and not the rows, so the cost is
    /// a parse and a walk per view.
    fn refresh_view_shapes(&self, session: &Session, database: &str) -> EngineResult<()> {
        let views: Vec<crate::catalog::View> = {
            let catalog = self.catalog()?;
            match catalog.database(database) {
                Some(db) => db.views.values().cloned().collect(),
                None => return Ok(()),
            }
        };

        let mut refreshed = Vec::new();
        for view in views {
            let mut scoped = session.clone();
            scoped.current_database = Some(database.to_owned());
            let Ok(resolved) =
                expr::probing(|| shaping(|| self.execute(&mut scoped, &view.definition, &[])))
            else {
                continue;
            };
            let Outcome::Rows(rows) = resolved else {
                continue;
            };
            // The guard is taken and dropped per view rather than held across
            // the loop: the `execute` above runs a query, which locks the
            // catalog itself.
            let shape = {
                let catalog = self.catalog()?;
                Self::view_shape(&catalog, &rows.columns, &view.columns)
            };
            refreshed.push((view.name.clone(), shape));
        }

        let mut catalog = self.catalog_mut()?;
        let Some(db) = catalog.database_mut(database) else {
            return Ok(());
        };
        for (name, shape) in refreshed {
            if let Some(view) = db.views.get_mut(&name) {
                view.shape = shape;
            }
        }
        Ok(())
    }

    /// The stored shape of a view, from the columns its definition produced.
    ///
    /// The catalog is read for one thing the projection does not carry: the
    /// collation the base column was declared with. 8.3 reports a view's
    /// column with the collation of the column behind it, and building the
    /// shape without it made a view over a `COLLATE utf8mb4_general_ci` column
    /// report the database default instead. The surrounding code copies `ty`
    /// and `nullable` across for the same reason.
    ///
    /// Only for a column that *is* a stored column: a computed one has no
    /// collation to inherit, and the grouped projection knows the column but
    /// not the table it came from — see `OutColumn::origin_table` — so it takes
    /// no collation rather than guessing at one.
    fn view_shape(
        catalog: &Catalog,
        columns: &[OutColumn],
        explicit: &[String],
    ) -> Vec<crate::catalog::Column> {
        columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut column = crate::catalog::Column::new(
                    explicit.get(i).cloned().unwrap_or_else(|| c.name.clone()),
                    // Always the materialised type, whether or not the view is
                    // one MySQL merges on read: a view's stored shape is built
                    // from the temporary-table field either way. Measured on
                    // 8.3, `information_schema.columns` calls a view over
                    // `ti + 1` an `int` while `SELECT` through the same view
                    // sends a LONGLONG, because reading it merges the
                    // expression back into the query and the declaration was
                    // fixed when the view was created.
                    c.materialised_sql_type(),
                );
                column.position = i + 1;
                column.nullable = c.nullable;
                column.collation = base_collation(catalog, c);
                column
            })
            .collect()
    }

    /// An owned copy of the whole catalog.
    ///
    /// The state image is the reason this is public: baking and inspecting an
    /// image are both operations on a catalog rather than on a connection.
    pub fn catalog_snapshot(&self) -> EngineResult<Catalog> {
        Ok(self.catalog()?.clone())
    }

    /// An engine holding an already-built catalog, as read from an image.
    ///
    /// A failure here has to be reported rather than swallowed: an engine that
    /// quietly kept its empty catalog would answer every query with "no such
    /// table" and look like a schema problem.
    pub fn from_catalog(catalog: Catalog, config: ServerConfig) -> EngineResult<Arc<Self>> {
        let engine = Self::new(config);
        *engine.catalog.write().map_err(|_| poisoned())? = catalog;
        Ok(engine)
    }

    /// Writes the whole catalog to a file.
    ///
    /// Taken under the writer gate as well as a read lock, so the image cannot
    /// catch a writer mid-statement: a writer computes against a copy and swaps
    /// it in at the end, and a snapshot between those two points would record a
    /// catalog that never existed.
    pub fn save_image(&self, path: &std::path::Path) -> EngineResult<()> {
        let _writing = self.begin_write()?;
        let bytes = image::encode(&*self.catalog()?);
        // Written to a neighbouring temporary and renamed, so a reader can
        // never see a half-written image — a test suite booting from one would
        // fail somewhere far from the cause.
        // The temporary is unique rather than `<name>.tmp`: two bake processes
        // writing neighbouring images in one directory would otherwise share a
        // name and truncate each other's half-written file.
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(format!(".{}.tmp", std::process::id()));
        let temporary = std::path::PathBuf::from(temporary);

        // Errors name the file that was asked for, not the temporary, which
        // the caller has never heard of.
        let written =
            std::fs::write(&temporary, &bytes).and_then(|()| std::fs::rename(&temporary, path));
        if let Err(e) = written {
            // Nothing should be left lying next to the target after a failure.
            let _ = std::fs::remove_file(&temporary);
            return Err(file_error(path, "write", &e));
        }
        Ok(())
    }

    /// Replaces the whole catalog with one read from a file.
    pub fn load_image(&self, path: &std::path::Path) -> EngineResult<()> {
        let bytes = std::fs::read(path).map_err(|e| file_error(path, "read", &e))?;
        let restored = image::decode(&bytes)?;
        let _writing = self.begin_write()?;
        *self.catalog_mut()? = restored;
        Ok(())
    }

    fn catalog(&self) -> EngineResult<std::sync::RwLockReadGuard<'_, Catalog>> {
        self.catalog.read().map_err(|_| poisoned())
    }

    fn catalog_mut(&self) -> EngineResult<std::sync::RwLockWriteGuard<'_, Catalog>> {
        self.catalog.write().map_err(|_| poisoned())
    }

    /// Serialises writers, held from before [`Engine::database_copy`] until
    /// after [`Engine::commit_database`].
    ///
    /// Without it a writer's copy can go stale between the two: another
    /// connection commits in the gap and the swap silently discards its rows,
    /// while both statements report success. Reads are unaffected — they take
    /// the catalog's read lock and never this.
    fn begin_write(&self) -> EngineResult<WriteGate<'_>> {
        // Re-entering would wait on a lock this thread already holds, forever,
        // and because the gate is global that wedges every other connection
        // too. Only `ultralight_snapshot` and `ultralight_restore` can reach
        // it from inside a statement, and both say so rather than hanging.
        if WRITING.with(std::cell::Cell::get) {
            return Err(EngineError::new(
                1105,
                "HY000",
                "this cannot be called from inside a statement that writes",
            ));
        }

        // A poisoned gate means a writer panicked mid-statement. The catalog
        // is untouched in that case, because a writer only swaps in at the
        // end, so taking the gate anyway is safe.
        let guard = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        WRITING.with(|held| held.set(true));
        Ok(WriteGate { _guard: guard })
    }

    /// A private copy of one database, to compute a write against.
    ///
    /// Every writer works on a copy and swaps it in at the end, and the reason
    /// is not only atomicity. Anything that can run a subquery — a scalar
    /// subquery in a VALUES list, a correlated one in SET or WHERE — reaches
    /// the executor, which takes a read lock to answer it. `RwLock` is not
    /// reentrant, so evaluating any of that while holding the write lock is a
    /// deadlock: the thread waits for a lock it holds itself, forever. Holding
    /// no lock while evaluating is what makes those statements possible.
    fn database_copy(&self, db: &str) -> EngineResult<Database> {
        self.catalog()?
            .database(db)
            .cloned()
            .ok_or_else(|| EngineError::unknown_database(db))
    }

    /// Puts a computed database back, replacing what was there.
    fn commit_database(&self, db: &str, working: Database) -> EngineResult<()> {
        let mut catalog = self.catalog_mut()?;
        let database = catalog
            .database_mut(db)
            .ok_or_else(|| EngineError::unknown_database(db))?;
        *database = working;
        Ok(())
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

    /// Read access for the DDL module, which lives outside this impl.
    pub(crate) fn catalog_public(&self) -> EngineResult<std::sync::RwLockReadGuard<'_, Catalog>> {
        self.catalog()
    }

    /// Fills a table `CREATE TABLE ... AS SELECT` is building with the rows
    /// its query produced.
    ///
    /// On the same path INSERT writes by, and that is the whole point of it
    /// being here rather than in the DDL module. Writing the rows straight
    /// into `table.rows` skipped every row-level rule the engine has, and 8.3
    /// applies all of them — measured, each one refusing the statement outright
    /// and leaving no table behind:
    ///
    /// | statement | 8.3 |
    /// |---|---|
    /// | `CREATE TABLE Dup (k INT PRIMARY KEY) AS SELECT 1 AS k UNION ALL SELECT 1` | 1062 |
    /// | `CREATE TABLE E (a INT NOT NULL) AS SELECT NULL AS a` | 1048 |
    /// | `CREATE TABLE C (id INT, CHECK (id > 100)) AS SELECT id FROM Src` | 3819 |
    /// | `CREATE TABLE A (seq INT AUTO_INCREMENT PRIMARY KEY) AS SELECT id FROM Src` | seq counts 1, 2 |
    ///
    /// The last is the one that had to be measured rather than assumed: an
    /// AUTO_INCREMENT column the query does not fill is assigned as if the
    /// rows had been inserted, so the counter advances and the next insert
    /// does not collide. It had been getting the type's implicit default —
    /// zero in every row, in a PRIMARY KEY column, with the counter still at
    /// one.
    ///
    /// Nothing is written to the catalog until this returns, so a refusal
    /// leaves no table, which is what 8.3 does too.
    ///
    /// `first` is where the query's columns begin: an explicit definition the
    /// query does not name sits in front of them and takes its default, the
    /// same as a column omitted from an INSERT.
    /// `fk_parents` is captured by the caller, under the read guard it drops
    /// before calling: taking it here would mean holding a second lock across
    /// the subqueries a CHECK or an expression default can run.
    pub(crate) fn fill_new_table(
        &self,
        session: &Session,
        table: &mut Table,
        mut fk_parents: Vec<foreign_keys::ParentKeys>,
        first: usize,
        coerce: &[bool],
        selected: Vec<Vec<Value>>,
    ) -> EngineResult<Option<u64>> {
        let ctx = Context::rowless(session).with_executor(self);
        // The *first* id generated, which is what `LAST_INSERT_ID()` answers
        // after a CTAS: 8.3 reports 1 for a two-row `CREATE TABLE t (s INT
        // AUTO_INCREMENT PRIMARY KEY) AS SELECT ...`, not 2.
        let mut first_id = None;

        for (nth, values) in selected.into_iter().enumerate() {
            // Counted like the rows of a VALUES list, for the same reason:
            // an error naming the row it is on is only right if something
            // keeps the count. `CREATE TABLE ... AS SELECT` is the caller
            // here; an `INSERT ... SELECT` materialises its rows and goes
            // through the VALUES loop.
            let _row = crate::types::WriteScope::row(nth + 1);
            let mut row = default_row(table, &ctx)?;
            for (slot, (value, coerce)) in (first..).zip(values.into_iter().zip(coerce)) {
                let column = &table.columns[slot];
                // A column derived from an expression carries only the
                // widening its wire type implies, so coercing into it would
                // narrow the value the expression already computed — see
                // `merge_selected_columns`.
                //
                // The flag is exactly "some DDL declared this column", which
                // is the condition `store` wants: 8.3 refuses
                // `CREATE TABLE t (v VARCHAR(3)) AS SELECT v FROM wide` and
                // leaves no table, where this stored six characters in a
                // column declared for three.
                row[slot] = if *coerce {
                    column.ty.store(value, &column.name)?
                } else {
                    value
                };
            }

            // Bound rather than discarded so this call site is its own: the
            // one INSERT makes is the same call with the same arguments.
            let generated = assign_auto_increment(table, &mut row)?;
            first_id = first_id.or(generated);

            let row = self.finalise_row(table, row, session)?;
            foreign_keys::check(&fk_parents, &row, &table.name)?;
            table.check_unique(&row, None)?;
            for parent in &mut fk_parents {
                parent.note_inserted(&row);
            }
            table.rows.push(row);
        }
        Ok(first_id)
    }

    /// Runs a query for the DDL module, which lives outside this impl.
    ///
    /// `CREATE TABLE ... AS SELECT` is defined by its query's *result*: MySQL
    /// declares the table from the result metadata and fills it with the rows,
    /// so the DDL needs exactly the answer a bare SELECT would have given.
    ///
    /// Not reconciled, and deliberately: `execute` reconciles on the way out
    /// only, because a value about to be sent has to match the code the client
    /// will decode it by, while an intermediate one should keep what it
    /// computed. These rows are on their way into a column, where `coerce`
    /// settles the same question against the type the column actually has.
    pub(crate) fn query_public(&self, session: &Session, query: &Query) -> EngineResult<ResultSet> {
        self.query(session, query)
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
        let _memo = SubqueryMemo::fresh();
        // One statement is one diagnostics area. Opened here and not deeper
        // because this is the boundary MySQL clears on: `EXECUTE` runs its
        // prepared statement through `execute_at_depth` below, and the two
        // together are one statement raising one set of conditions.
        let collecting = crate::diagnostics::Collecting::open(session.max_error_count() as usize);
        let ran = self.execute_at_depth(session, sql, parameters, 0);
        // The statement's own error is a condition too, and the last one:
        // under `STRICT_ALL_TABLES` an INSERT whose first row trims trailing
        // spaces and whose second is too long leaves a Note 1265 and then an
        // Error 1406, in that order.
        if let Err(e) = &ran {
            crate::diagnostics::raise(crate::diagnostics::Level::Error, e.code, e.message.clone());
        }
        let (raised, keep) = collecting.taken();
        // A reader leaves the area exactly as it found it, so the same list
        // can be read twice; everything else replaces it, with nothing when it
        // raised nothing. See `crate::diagnostics`.
        //
        // A statement that *failed* replaces it whatever it asked for. Nothing
        // reaches that today -- the readers cannot fail after saying `keep` --
        // and the alternative is an error the client is never told about,
        // which is the one outcome worth spending a condition on.
        if !keep || ran.is_err() {
            session.set_diagnostics(raised);
        }
        let mut outcome = ran?;
        // On the way out only: an intermediate result feeds another expression
        // and should keep exactly the value it computed, where a result that
        // is about to be sent has to match what the client will decode it by.
        if let Outcome::Rows(rows) = &mut outcome {
            rows.reconcile();
        }
        Ok(outcome)
    }

    /// Runs a command that may hold several statements, one outcome each.
    ///
    /// Prisma's migration engine sends a whole migration file as one command,
    /// so this is what makes `prisma migrate deploy` possible. Execution stops
    /// at the first error: the statements after it were written expecting the
    /// earlier ones to have succeeded.
    pub fn execute_many(&self, session: &mut Session, sql: &str, parameters: &[Value]) -> Executed {
        let statements = rewrite::split_statements(sql);
        if statements.len() <= 1 {
            // One statement is the overwhelmingly common case, and routing it
            // through the splitter's output would re-render text the single
            // path already handles verbatim.
            return match self.execute(session, sql, parameters) {
                Ok(outcome) => {
                    let warnings = vec![session.diagnostics().wire_count()];
                    Executed::of(vec![outcome], warnings)
                }
                Err(e) => Executed::failed(Vec::new(), Vec::new(), e),
            };
        }

        // Parameters belong to a prepared statement, which is always one
        // statement; a multi-statement command carrying them is malformed.
        if !parameters.is_empty() {
            return Executed::failed(
                Vec::new(),
                Vec::new(),
                unsupported("parameters with a multi-statement command"),
            );
        }

        let mut outcomes = Vec::with_capacity(statements.len());
        let mut warnings = Vec::with_capacity(statements.len());
        for statement in statements {
            match self.execute(session, &statement, &[]) {
                Ok(outcome) => {
                    outcomes.push(outcome);
                    // Read here, while it is still this statement's: the next
                    // one replaces the area.
                    warnings.push(session.diagnostics().wire_count());
                }
                // The statements before the failure have already been applied,
                // and there is no rollback here. Returning them with the error
                // is what lets a caller see how far the command got, which for
                // a migration is the difference between "did nothing" and
                // "half applied".
                Err(e) => return Executed::failed(outcomes, warnings, e),
            }
        }
        Executed::of(outcomes, warnings)
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

        let bound = rewrite::bind(sql, parameters);
        let mode = session.sql_mode();

        // Statements the parser cannot take are recognised before it sees them.
        if let Some(intercepted) = rewrite::intercept(&bound.sql) {
            return self.run_intercepted(session, intercepted, depth);
        }

        let normalised = std::rc::Rc::new(rewrite::normalise(bound, mode));
        let _source = Source::set(&normalised, session);
        let statements = Parser::parse_sql(&MySqlDialect {}, &normalised.sql)
            .map_err(|e| EngineError::parse(format!("{e}")))?;

        match statements.len() {
            0 => Ok(Outcome::ok()),
            1 => self.run(session, &statements[0]),
            // Reached only when a caller uses `execute` directly on several
            // statements. It can return one outcome, so anything past the
            // first would be run and then silently discarded.
            _ => Err(unsupported(
                "several statements here; use a multi-statement command",
            )),
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
                    None => rewrite::unquote_literal(&source),
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
                let (db, table_name) = self.qualify(session, table)?;
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

            Intercepted::ShowDiagnostics(show) => {
                // Said before the rows are built, not after: this is the one
                // family of statements that reads the diagnostics area
                // without replacing it, and saying so is what leaves the list
                // there for the next reader.
                crate::diagnostics::keep_area();
                Ok(Outcome::Rows(show_diagnostics(session, &show)))
            }
        }
    }

    fn run(&self, session: &mut Session, statement: &Statement) -> EngineResult<Outcome> {
        match statement {
            // A statement can be a query with a WITH clause whose body is a
            // write: `WITH x AS (...) UPDATE t JOIN x ...`. The CTEs are
            // materialised first either way, so the body sees them by name.
            Statement::Query(query) => match query.body.as_ref() {
                SetExpr::Update(inner) | SetExpr::Insert(inner) | SetExpr::Delete(inner) => {
                    let mut scoped = session.clone();
                    self.materialise_ctes(&mut scoped, query)?;
                    let outcome = self.run(&mut scoped, inner)?;
                    // The CTEs go out of scope with the statement; nothing else
                    // the body changed should be discarded with them.
                    scoped.ctes.clear();
                    *session = scoped;
                    Ok(outcome)
                }
                _ => self.query(session, query).map(Outcome::Rows),
            },
            Statement::Insert(insert) => self.insert(session, insert),
            Statement::Update(update) => {
                // Anything not read below would be silently dropped, which for
                // LIMIT means updating every row instead of one.
                if update.from.is_some() {
                    return Err(unsupported("UPDATE ... FROM"));
                }
                if update.returning.is_some() {
                    return Err(unsupported("RETURNING"));
                }
                self.update(
                    session,
                    &update.table,
                    &update.assignments,
                    update.selection.as_ref(),
                    &update.order_by,
                    update.limit.as_ref(),
                )
            }
            Statement::Delete(delete) => self.delete(session, delete),

            Statement::CreateTable(create) => ddl::create_table(self, session, create),
            Statement::CreateView(create) => {
                // The view's own name is an identifier like any other, and its
                // columns are not: 8.3 answers a 65-character view name with
                // 1059 and a 65-character column — written in the column list
                // or as an alias anywhere in the definition — with 1166.
                for part in name::parts(&create.name) {
                    name::check_identifier(&part)?;
                }
                for column in &create.columns {
                    name::check_column_name(&column.name.value)?;
                }
                check_aliases_in_query(&create.query)?;

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
                // Before the write guard below, which this would deadlock
                // against.
                let shape = {
                    let catalog = self.catalog()?;
                    Self::view_shape(&catalog, &resolved.columns, &explicit)
                };

                let view = crate::catalog::View {
                    name: name.clone(),
                    definition: pinned_definition(&create.query),
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
            Statement::AlterTable(alter) => {
                let outcome = ddl::alter_table(
                    self,
                    session,
                    &alter.name,
                    &alter.operations,
                    alter.if_exists,
                )?;
                // A view's shape is worked out once and stored, so widening a
                // column leaves every view over it describing the old width.
                // MySQL re-resolves a view when it is opened and reports the
                // new one. Done after the DDL rather than inside it, because
                // resolving runs a query and the DDL holds the write lock.
                let (db, _) = self.split_name(session, &alter.name)?;
                self.refresh_view_shapes(session, &db)?;
                Ok(outcome)
            }
            Statement::CreateIndex(create) => ddl::create_index(self, session, create),
            Statement::CreateDatabase {
                db_name,
                if_not_exists,
                ..
            } => {
                // 8.3 refuses this before `IF NOT EXISTS` can excuse it.
                for part in name::parts(db_name) {
                    name::check_identifier(&part)?;
                }
                self.catalog_mut()?
                    .create_database(&name::dotted(db_name), *if_not_exists)?;
                Ok(Outcome::ok())
            }
            Statement::Drop {
                object_type,
                names,
                if_exists,
                table,
                ..
            } => self.drop(session, *object_type, names, *if_exists, table.as_ref()),
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
                // The name out of the tree, rather than out of
                // `format!("{use_stmt}")` with the `USE ` this statement had
                // just been rendered with stripped back off the front — which
                // put a quoted name into the session under its rendering, so
                // ``USE `db``x` `` selected a database called ``db``x``.
                //
                // The other `Use` variants are `USE CATALOG`, `USE WAREHOUSE`
                // and the rest, which only Databricks and Snowflake parse:
                // MySQL's dialect reads no keyword after `USE` at all, so
                // every one of these is `Object` and the arm below cannot be
                // reached. It refuses rather than guessing a database name out
                // of the rendering, which is what the old reading did.
                let sqlparser::ast::Use::Object(object) = use_stmt else {
                    return Err(unsupported(format!("{use_stmt}")));
                };
                let name = name::dotted(object);
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
                    .map(name::dotted)
                    .unwrap_or_default();
                self.show_columns(session, &table).map(Outcome::Rows)
            }

            Statement::StartTransaction { .. } => {
                // A nested BEGIN commits the open transaction in MySQL rather
                // than nesting, so the snapshot is simply replaced.
                session.transaction = Some(Box::new(crate::session::Transaction {
                    opened: self.catalog()?.clone(),
                    savepoints: Vec::new(),
                    modified: Vec::new(),
                }));
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
        self.query_outer(session, query, None)
    }

    /// A whole query's answer, against an enclosing row where there is one.
    ///
    /// Split from [`Engine::query`] for `subquery_column`, which asks this
    /// question of a *correlated* scalar subquery and needs the boundary
    /// below rather than `query_with_outer`'s: the materialisation is what
    /// gives an all-NULL set operation its `binary(0)`, and a scalar subquery
    /// takes the type the inner query answers with.
    fn query_outer(
        &self,
        session: &Session,
        query: &Query,
        outer: Option<&OuterRow>,
    ) -> EngineResult<ResultSet> {
        let mut result = self.query_with_outer(session, query, outer)?;
        // A set operation's answer goes through a temporary table, and a
        // temporary table has no field for `MYSQL_TYPE_NULL`, so a column that
        // was NULL on every branch comes out `binary(0)` — measured on 8.3,
        // and confirmed by `CREATE TABLE t AS SELECT NULL UNION SELECT NULL`,
        // whose column is declared exactly that.
        //
        // Here rather than inside the merge, because MySQL materialises a
        // chain of set operations *once*, over all its terms at the same time.
        // `SELECT NULL UNION SELECT NULL UNION SELECT 1` is a LONGLONG there —
        // and so is the same statement with the first two bracketed, so it is
        // not associativity that decides it. Converting inside the merge made
        // the left pair a `binary(0)` that then widened against the 1 to a
        // string, and a binary client read "1" for the integer 1.
        //
        // `query` is the boundary a whole query answers at, and a set
        // operation nested in another one does not come through it: a
        // bracketed arm is a `SetExpr::Query` handled by `query_with_outer`.
        // The other way a set operation is materialised — as a derived table,
        // a CTE or a view — is `SqlType::from_column_type`, which every one of
        // those reads its columns back through.
        if is_set_operation(query.body.as_ref()) {
            for column in &mut result.columns {
                if column.ty == ColumnType::Null {
                    let materialised = crate::types::SqlType::materialised_null();
                    column.ty = materialised.column_type();
                    column.declared = Some(materialised);
                }
            }
        }
        Ok(result)
    }

    /// Runs each CTE of a query and puts its rows in scope under its name.
    ///
    /// Each is evaluated once, and can see the ones declared before it, which
    /// is what MySQL does for a non-recursive WITH. A CTE of a `WITH
    /// RECURSIVE` that names itself is run repeatedly instead; see
    /// [`Self::recursive_cte`].
    fn materialise_ctes(&self, session: &mut Session, query: &Query) -> EngineResult<()> {
        let Some(with) = &query.with else {
            return Ok(());
        };

        // 8.3 does not look at a `WITH RECURSIVE` entry nothing reads at all —
        // it neither runs it nor resolves it. Measured: a statement carrying a
        // runaway recursive CTE the body never names answers, where running it
        // would be 3636, and so does one carrying a *malformed* recursive CTE,
        // where resolving it would be 3574. So such an entry is skipped whole
        // rather than planned and refused.
        //
        // Computed once for the whole list rather than per entry, because
        // reachability is transitive and an entry cannot answer it alone. See
        // `recursive::read_ctes`, which errs towards "read".
        let read = if with.recursive {
            crate::recursive::read_ctes(query)
        } else {
            Vec::new()
        };

        for (position, cte) in with.cte_tables.iter().enumerate() {
            let name = cte.alias.name.value.clone();
            if with.recursive && !read[position] {
                continue;
            }
            // Only under `WITH RECURSIVE`: without the keyword a CTE naming
            // itself is naming a *table*, and 8.3 answers 1146 where the table
            // does not exist rather than recursing.
            let plan = if with.recursive {
                crate::recursive::plan(cte, &name)?
            } else {
                None
            };
            let relation = match plan {
                // Renamed already: a recursive branch reads the CTE by the
                // alias's column names, so `recursive_cte` has to apply them
                // before it iterates rather than after.
                Some(plan) => self.recursive_cte(session, cte, &name, &plan)?,
                None => {
                    let mut plain = self.relation_of_query(session, &cte.query, &name)?;
                    rename_cte_columns(&mut plain, cte, &name)?;
                    plain
                }
            };
            session.ctes.push((name, relation));
        }
        Ok(())
    }

    /// Runs a recursive CTE: the anchor once, then the recursive branches
    /// against the rows the previous round added, until a round adds none.
    ///
    /// Three things here are measured on 8.3 rather than derived, and each of
    /// them is a different answer if it is guessed:
    ///
    /// * **The anchor alone types the CTE.** Every row a later round produces
    ///   is written into a field the anchor's column declared, so a recursion
    ///   that outgrows it is an error and not a wider column: over a `TINYINT`
    ///   anchor holding 100, stepping by ten to 130 is 1264, and over
    ///   `CAST(1 AS UNSIGNED)`, stepping down past zero is 1690. The scale
    ///   travels the same way — a `DECIMAL(10,2)` anchor answers `1.50` for a
    ///   round that computed `1.5`.
    /// * **`UNION` de-duplicates against everything produced so far**, not
    ///   only against the round that produced it, which is what makes a cycle
    ///   in the data terminate.
    /// * **The CTE's own `LIMIT` stops the recursion**, and stops it before
    ///   the depth limit can fire: at `cte_max_recursion_depth = 2`,
    ///   `... UNION ALL SELECT n + 1 FROM t LIMIT 3` answers 1, 2, 3 and the
    ///   same statement with `LIMIT 4` is 3636.
    ///
    /// Only reached for a CTE something reads: `materialise_ctes` skips one
    /// nothing names, because 8.3 does not look at it either.
    ///
    /// The write into the anchor's field is [`crate::types::SqlType::coerce`],
    /// which is short of MySQL in two ways this path inherits rather than
    /// causes. Both are visible without a CTE at all, which is what says they
    /// belong to `coerce`: `INSERT INTO t (s VARCHAR(3)) VALUES ('abcdef')`
    /// stores the six characters where 8.3 is 1406, and `INSERT INTO t
    /// (i INT) VALUES (1.5), (2.5), (-1.5)` stores 1, 2 and -1 where 8.3
    /// rounds away from zero to 2, 3 and -2. So a recursion that outgrows a
    /// narrow anchor answers rows here and 1406 there, and one that steps by a
    /// fraction steps by the truncated value.
    fn recursive_cte(
        &self,
        session: &Session,
        cte: &sqlparser::ast::Cte,
        name: &str,
        plan: &crate::recursive::Plan,
    ) -> EngineResult<Relation> {
        // The CTE's *own* WITH clause, which its branches read by name.
        // `recursive::plan` flattened the body, so the clause is part of
        // neither the anchor nor the step and every branch below is handed to
        // `set_expr` without it — `WITH RECURSIVE t AS (WITH x AS (SELECT 1 AS
        // n) SELECT n FROM x UNION ALL SELECT n + 1 FROM t ...)` answered 1146
        // for `x`. The ordinary CTE path never had this: it goes through
        // `query`, which materialises a nested WITH on the way in.
        let mut inner;
        let session = if cte.query.with.is_some() {
            inner = session.clone();
            self.materialise_ctes(&mut inner, &cte.query)?;
            &inner
        } else {
            session
        };

        let mut anchor = self.set_expr(session, &plan.anchor, None)?;
        if plan.distinct {
            dedupe(&mut anchor.rows);
        }
        // The alias's column names before anything else, because the recursive
        // branch references the CTE *by them*: `WITH RECURSIVE seq(n) AS
        // (SELECT 1 UNION ALL SELECT n + 1 FROM seq ...)` has no column called
        // `n` until the rename has happened, and answers 1054 without it.
        let mut scratch = self.relation_from_result(
            session,
            &anchor.columns,
            anchor.rows.clone(),
            &cte.query,
            name,
        );
        rename_cte_columns(&mut scratch, cte, name)?;
        // Every column of a recursive CTE is nullable, whatever the anchor
        // declared. Measured on 8.3 rather than reasoned about: over a folder
        // table whose `id` is a primary key and whose `name` is NOT NULL, both
        // columns come back without NOT_NULL through a recursive CTE, and the
        // same CTE without a recursive branch keeps the flag on both. The
        // recursive branch is what does it — it names the CTE, so its own
        // nullability cannot be settled without already knowing the answer,
        // and MySQL settles the cycle by taking the relaxed side. A NOT_NULL
        // sent here is what a generated client reads a field's optionality
        // from.
        for column in &mut scratch.columns {
            column.nullable = true;
        }

        // The fields every produced row is written through, taken from the
        // anchor and never widened afterwards. Kept with their names because
        // that is what an over-long value's 1406 reports.
        let fields: Vec<(String, crate::types::SqlType)> = scratch
            .columns
            .iter()
            .map(|c| (c.name.clone(), c.ty.clone()))
            .collect();

        // How far it is worth recursing at all. `None` for no LIMIT; the
        // offset is counted in because the rows it skips still have to be
        // produced before the rows it keeps can be.
        let reach = limit_reach(session, &cte.query)?;
        let depth = session.cte_max_recursion_depth();

        let mut added = std::mem::take(&mut scratch.rows);
        let mut seen: std::collections::HashSet<Vec<String>> = if plan.distinct {
            added.iter().map(|row| row_key(row)).collect()
        } else {
            std::collections::HashSet::new()
        };
        let mut rows = added.clone();
        let mut round = 0u64;

        // Cloned once, not once per round. The session carries every CTE
        // already in scope, so cloning it inside the loop copied all of their
        // rows on every iteration: a 900-round recursion beside a 2000-row
        // sibling CTE took 292ms where the same recursion alone took 3.7ms.
        // The recursive CTE's own entry is the last one and is the only thing
        // that changes, so the loop replaces that entry rather than the
        // session.
        let mut scoped = session.clone();
        scoped.ctes.push((name.to_owned(), scratch.clone()));
        let bound = scoped.ctes.len() - 1;

        // An anchor that produced nothing ends it here, without an iteration
        // and so without a depth limit to exceed: measured on 8.3, an empty
        // anchor over a runaway recursive branch answers no rows at
        // `cte_max_recursion_depth = 0` rather than 3636.
        while !added.is_empty() && reach.is_none_or(|reach| rows.len() < reach) {
            round += 1;
            if round > depth {
                return Err(EngineError::new(
                    3636,
                    "HY000",
                    format!(
                        "Recursive query aborted after {round} iterations. Try increasing \
                         @@cte_max_recursion_depth to a larger value."
                    ),
                ));
            }

            // Only the rows the last round added, which is what makes this a
            // fixpoint rather than a re-run: feeding the whole result back
            // produces every row again on every round, and a `UNION ALL` that
            // terminates would stop terminating.
            scratch.rows = added;
            // In step with the rows, always. A join reads `origins` by row
            // index to know which stored row a joined one came from, so a
            // round that adds more rows than the anchor had left the shorter
            // vector behind and the next join indexed past its end — a panic
            // in the middle of a user's statement rather than an answer.
            scratch.origins = vec![Vec::new(); scratch.rows.len()];
            scoped.ctes[bound].1 = scratch.clone();
            let produced = self.set_expr(&scoped, &plan.step, None)?;
            // The two halves of the UNION are evaluated apart, so nothing else
            // compares their widths — and a mismatch would otherwise be
            // silently zipped away, padding a row with NULL or dropping a
            // column. 8.3 answers 1222 for either direction.
            if produced.columns.len() != fields.len() {
                return Err(EngineError::new(
                    1222,
                    "21000",
                    "The used SELECT statements have a different number of columns",
                ));
            }

            added = Vec::with_capacity(produced.rows.len());
            for row in materialised(produced.rows) {
                let mut written = Vec::with_capacity(fields.len());
                for (value, (column, ty)) in row.into_iter().zip(&fields) {
                    written.push(ty.coerce(value, column)?);
                }
                // Against everything produced so far and not against this
                // round, which is the whole of what stops a cycle in the data.
                if plan.distinct && !seen.insert(row_key(&written)) {
                    continue;
                }
                added.push(written);
            }
            rows.extend(added.iter().cloned());
        }

        anchor.rows = rows;
        apply_limit(&mut anchor, session, &cte.query)?;
        scratch.origins = vec![Vec::new(); anchor.rows.len()];
        scratch.rows = anchor.rows;
        Ok(scratch)
    }

    /// Runs a query, optionally against an enclosing row so a correlated
    /// subquery can reference the outer query's columns.
    fn query_with_outer(
        &self,
        session: &Session,
        query: &Query,
        outer: Option<&OuterRow>,
    ) -> EngineResult<ResultSet> {
        // A WITH clause is materialised here rather than at any one caller,
        // because every path that runs a query — a subquery, a view body, a
        // derived table, the top level — comes through this function. Doing it
        // in `query` alone left a CTE inside a subquery reported as a missing
        // table.
        if query.with.is_some() {
            let mut scoped = session.clone();
            // The enclosing row is put in scope before the entries are run,
            // the same way `select` puts it in scope before a FROM is built: a
            // CTE belongs to the query block that writes it, so one written
            // inside a correlated subquery may name the row that subquery is
            // being run for. Measured: `SELECT k FROM Cl WHERE EXISTS (WITH q
            // AS (SELECT 1 FROM Cr JOIN Cr c3 ON c3.id = Cl.id) SELECT 1 FROM
            // q)` answers on 8.3 and was 1054 in the ON here, because the
            // entry was materialised before anything had put the row anywhere
            // the join could read it.
            if let Some(outer) = outer {
                scoped.lateral = Some(outer.clone());
            }
            self.materialise_ctes(&mut scoped, query)?;
            let mut without = query.clone();
            // Cleared so the recursive call does not run them a second time.
            without.with = None;
            return self.query_with_outer(&scoped, &without, outer);
        }

        let select = match query.body.as_ref() {
            SetExpr::Select(select) => select,
            // A set operation runs both sides and combines them, so it is
            // handled before the single-SELECT path.
            SetExpr::SetOperation { .. } | SetExpr::Query(_) | SetExpr::Values(_) => {
                let mut combined = self.set_expr(session, query.body.as_ref(), outer)?;
                check_projected_order(&combined.columns, query.order_by.as_ref(), outer)?;
                self.order_projected(&mut combined, session, query.order_by.as_ref(), &[])?;
                apply_limit(&mut combined, session, query)?;
                return Ok(combined);
            }
            other => return Err(unsupported(format!("query body `{other}`"))),
        };

        // Ordering by an output alias rather than a source column is legal and
        // cannot be resolved against the table; `select` finishes that itself,
        // where the keys the source rows *did* answer are still to hand.
        let mut rows = self.select(session, select, query.order_by.as_ref(), outer)?;
        apply_limit(&mut rows, session, query)?;
        Ok(rows)
    }

    /// A `VALUES ROW(...), ROW(...)` constructor as a result set.
    ///
    /// MySQL names the columns `column_0`, `column_1`, and so on, which is what
    /// anything selecting from it has to reference.
    fn values_result(
        &self,
        session: &Session,
        values: &sqlparser::ast::Values,
    ) -> EngineResult<ResultSet> {
        let sub = Some(Sub {
            engine: self,
            session,
        });
        let ctx = Context::rowless(session).with_executor(self);
        let mut rows: Vec<Vec<Value>> = Vec::with_capacity(values.rows.len());
        for row in &values.rows {
            let mut out = Vec::with_capacity(row.content.len());
            for e in &row.content {
                out.push(expr::eval(e, &ctx)?);
            }
            rows.push(out);
        }
        // A table value constructor is a table-shaped result like a derived
        // table or a CTE, so it takes the boolean tag off for the same reason
        // they do — the value is given a column to live in. Before the columns
        // are derived, not after: `wire_type` below reads the first row, and a
        // boolean is not a `Value::Int`, so `VALUES ROW(TRUE)` fell to its
        // VAR_STRING catch-all and was described a string while carrying 1.
        // MySQL types that constructor's `column_0` LONGLONG.
        let rows = materialised(rows);

        let width = rows.first().map(|r| r.len()).unwrap_or(0);
        // A ragged constructor would produce rows the wire cannot frame, and
        // MySQL refuses it outright.
        if rows.iter().any(|r| r.len() != width) {
            return Err(EngineError::new(
                1136,
                "21S01",
                "Column count doesn't match value count",
            ));
        }

        let columns = (0..width)
            .map(|i| {
                // Folded over every row rather than read off the first, and
                // through the same `widen` a set operation uses, because a
                // constructor's rows share one column exactly as a union's
                // branches do. `VALUES ROW(1), ROW(18446744073709551615)` is a
                // NEWDECIMAL on 8.3 — there is no 64-bit integer holding both
                // — and taking the first row's answer would have announced a
                // signed LONGLONG over a value the engine sends unsigned.
                let (ty, unsigned) = rows
                    .iter()
                    .filter_map(|r| r.get(i))
                    // NULL says nothing about the others, exactly as it says
                    // nothing in `combined`: `VALUES ROW(1), ROW(NULL)` is a
                    // LONGLONG on 8.3, and folding NULL's VAR_STRING in would
                    // have made it a string.
                    .filter(|v| !v.is_null())
                    .map(|v| (wire_type(v), matches!(v, Value::Unsigned(_))))
                    .reduce(|(a, a_unsigned), (b, b_unsigned)| {
                        (
                            widen(a, a_unsigned, b, b_unsigned),
                            a_unsigned && b_unsigned,
                        )
                    })
                    // Nothing left to fold means every row was NULL in this
                    // column, and NULL is the absence of a type rather than a
                    // string: measured on 8.3, `VALUES ROW(NULL)` is
                    // `MYSQL_TYPE_NULL`, exactly as `SELECT NULL` is. The
                    // VAR_STRING that was here made it a `varchar(512)` the
                    // moment anything read it back as a relation.
                    //
                    // Left typeless rather than materialised here, because a
                    // bare constructor is not materialised — MySQL gives
                    // `SELECT * FROM (VALUES ROW(NULL)) v` `binary(0)` and the
                    // constructor on its own `MYSQL_TYPE_NULL`. Saying `Null`
                    // is what lets the two materialising paths give it the
                    // other answer where they apply.
                    .unwrap_or((ColumnType::Null, false));
                let mut column = OutColumn::new(format!("column_{i}"), ty);
                column.unsigned = unsigned;
                // From the expressions rather than from the values, which is
                // where every other projection reads it: MySQL describes
                // `VALUES ROW(1)` NOT NULL, and the column was announced
                // nullable whatever it held. A constructor is NOT NULL only
                // if *every* row's expression is, since they share one column.
                column.nullable = !values.rows.iter().all(|row| {
                    row.content
                        .get(i)
                        .is_some_and(|e| never_null(e, &[], &[], sub))
                });
                column
            })
            .collect();

        Ok(ResultSet { columns, rows })
    }

    /// Evaluates a set operation, or a nested query body.
    fn set_expr(
        &self,
        session: &Session,
        body: &SetExpr,
        outer: Option<&OuterRow>,
    ) -> EngineResult<ResultSet> {
        match body {
            SetExpr::Select(select) => self.select(session, select, None, outer),
            SetExpr::Query(query) => self.query_with_outer(session, query, outer),
            // MySQL 8's table value constructor: `VALUES ROW(1), ROW(2)` is a
            // result in its own right, usable anywhere a SELECT is.
            SetExpr::Values(values) => self.values_result(session, values),
            SetExpr::SetOperation {
                left,
                op,
                set_quantifier,
                right,
            } => {
                use sqlparser::ast::{SetOperator, SetQuantifier};

                let mut left = self.set_expr(session, left, outer)?;
                let mut right = self.set_expr(session, right, outer)?;

                if left.columns.len() != right.columns.len() {
                    return Err(EngineError::new(
                        1222,
                        "21000",
                        "The used SELECT statements have a different number of columns",
                    ));
                }

                // All three operators keep the first branch's column names,
                // and all three remove duplicates unless ALL is given.
                //
                // The declared *type* is only kept where both branches agree.
                // A column fed by `SMALLINT` on one side and `BIGINT` on the
                // other holds values from both, so reporting the left one
                // describes a column narrower than what it returns — MySQL
                // aggregates the two into the wider type, and dropping back to
                // the widening the wire type implies is the honest version of
                // that without a type lattice.
                for (i, column) in left.columns.iter_mut().enumerate() {
                    // Whatever the branches were, the merged column is not an
                    // expression any more and has no width of one. Measured on
                    // 8.3: `SELECT 1 MOD 3 UNION SELECT 4` materialises -- it
                    // loses the BINARY flag -- and stays a LONGLONG, because a
                    // set operation aggregates the branches' *types* and never
                    // their widths. Carried through, the left branch's width
                    // narrowed the merged column: `CREATE VIEW v AS SELECT 1
                    // UNION SELECT 9223372036854775807` declared an `int` over
                    // a column that returns 2^63-1, and `COLUMN_TYPE` is what
                    // Prisma reads to pick a field type. Cleared before the
                    // NULL arms below, so every path out of this loop drops
                    // it.
                    column.max_length = None;
                    let Some(right) = right.columns.get(i) else {
                        continue;
                    };
                    // The places are not dropped with the width, because a set
                    // operation aggregates them: measured on 8.3, `SELECT a + b
                    // ... UNION ALL SELECT a - b` over a `DECIMAL(10,2)` and a
                    // `DECIMAL(7,3)` is `dec 3`, the same three places each
                    // branch has. The wider of the two, for the reason
                    // `nullable` takes either branch's — a column that must
                    // describe both cannot show fewer places than one of them
                    // carries.
                    column.decimals = column.decimals.max(right.decimals);
                    // A branch that can produce NULL makes the whole column
                    // able to, whatever the other side declared. Only visible
                    // once NOT NULL was reported at all: while every string
                    // column was nullable this was right by accident.
                    column.nullable |= right.nullable;

                    // A NULL branch is the absence of a type, not a competing
                    // one. MySQL keeps whatever the other branch is — its
                    // width, its sign and its declared spelling — and relaxes
                    // only the nullability, which the line above has done.
                    // Widening against it instead fell through to the string
                    // fallback, so `SELECT id FROM t UNION SELECT NULL` came
                    // back a VAR_STRING and a binary client read "1" where
                    // MySQL sends the integer 1.
                    if right.ty == ColumnType::Null {
                        continue;
                    }
                    if column.ty == ColumnType::Null {
                        column.ty = right.ty;
                        column.unsigned = right.unsigned;
                        column.declared = right.declared.clone();
                        continue;
                    }

                    // Signedness counts as a difference even when the codes
                    // agree: an `INT` and an `INT UNSIGNED` are both `LONG`
                    // and neither range holds the other's.
                    //
                    // Read before the flags are merged, not after. Narrowing
                    // the column to signed first made the two sides *match*,
                    // so a mixed-sign union took the "same code, keep it" path
                    // and stayed a LONG where MySQL widens to LONGLONG.
                    if right.ty == column.ty && right.unsigned == column.unsigned {
                        // Same code on both sides, so the column keeps it —
                        // and keeps a declared type, which is the only thing
                        // that says whether a `BLOB` carries text. Clearing it
                        // here made `TEXT UNION LONGTEXT` a *binary* blob, the
                        // very failure the type codes were narrowed to fix.
                        //
                        // The wider of the two declarations, because MySQL
                        // takes the wider whichever side it is on: a
                        // `VARCHAR(12) UNION VARCHAR(40)` is a `varchar(40)`
                        // read in either order.
                        let width = |c: &Option<crate::types::SqlType>| {
                            c.as_ref().and_then(|d| d.length).unwrap_or(0)
                        };
                        // Read before the width swap below overwrites it, for
                        // the reason the signedness above is read before the
                        // flags are merged: the merge needs both sides, and
                        // the swap has already thrown one of them away.
                        // Without this, `FLOAT(10,4) UNION ALL FLOAT(12,2)`
                        // merged the wider declaration with itself and showed
                        // two places, while the same pair the other way round
                        // showed four.
                        let merged =
                            merged_approximate(column.ty, &column.declared, &right.declared);
                        if width(&right.declared) > width(&column.declared) {
                            column.declared = right.declared.clone();
                        }
                        // Except for an approximate scale, which is the *max*
                        // and not the wider declaration's: measured on 8.3,
                        // `FLOAT(12,2) UNION ALL FLOAT(10,4)` is `float(14,4)`
                        // read in either order, where taking the wider side
                        // whole answers two places one way round and four the
                        // other.
                        if merged.is_some() {
                            column.declared = merged;
                        }
                        continue;
                    }
                    // Widened, not taken from the left. The column holds
                    // values from both branches, and the binary protocol
                    // encodes by the declared code — so calling a
                    // `SMALLINT UNION BIGINT` a SMALLINT would write two bytes
                    // for an eight-byte value. MySQL aggregates the two into
                    // the wider type and this does the same, falling back to a
                    // string where there is no wider number, which is what it
                    // does for `SMALLINT UNION VARCHAR`.
                    column.ty = widen(column.ty, column.unsigned, right.ty, right.unsigned);
                    // Both branches have to be unsigned for the column to be,
                    // or `SELECT iu FROM t UNION SELECT i FROM t` goes out
                    // UNSIGNED and hands a binary client 18446744073709551611
                    // for -5. Only on this path: the branch above returns when
                    // the two already agree.
                    column.unsigned &= right.unsigned;
                    // Cleared, except where both branches are approximate and
                    // declare a scale: that scale is the whole difference
                    // between `0.1000` and `0.10000000149011612`, and the
                    // widening is exactly where it used to be dropped —
                    // `FLOAT(10,4) UNION ALL DOUBLE(10,4)` kept it while
                    // `FLOAT(10,4) UNION ALL DOUBLE(12,6)` did not, because
                    // only the second one changes the code.
                    column.declared =
                        merged_approximate(column.ty, &column.declared, &right.declared);
                }

                // A set operation materialises its result, and MySQL's
                // temporary table has one field per column: values from both
                // branches are written into it and read back at *its* scale.
                // That is visible before anything is printed —
                // `SELECT f FROM t UNION SELECT d FROM t` over a `FLOAT(10,4)`
                // and a `DOUBLE(10,4)` both holding 0.1 is one row on 8.3, and
                // two without this, because the widened four-byte value and
                // the eight-byte one differ until they are rounded together.
                // So it happens here, ahead of the de-duplication below.
                let scaled: Vec<Option<(String, crate::types::SqlType)>> = left
                    .columns
                    .iter()
                    .map(|c| {
                        let declared = c.declared.clone().filter(|d| {
                            d.kind == crate::types::TypeKind::Float && d.scale.is_some()
                        })?;
                        Some((c.name.clone(), declared))
                    })
                    .collect();
                for (i, merged) in scaled.iter().enumerate() {
                    let Some((name, declared)) = merged else {
                        continue;
                    };
                    for row in left.rows.iter_mut().chain(right.rows.iter_mut()) {
                        let Some(value) = row.get_mut(i) else {
                            continue;
                        };
                        // Cannot fail: the merged width keeps the widest
                        // integer part of either branch, so every value that
                        // fit the column it came from fits this one.
                        let taken = std::mem::replace(value, Value::Null);
                        *value = declared.coerce(taken, name)?;
                    }
                }

                // A decimal column's scale is aggregated across the branches
                // the same way, and the temporary table's field holds every
                // branch's values at it. That is #0123: measured on 8.3,
                // `SELECT 2.50 UNION ALL SELECT 2.5 UNION ALL SELECT 2` is
                // 2.50, 2.50 and 2.00 there where this engine showed each
                // branch at its own. An integer branch is written at the
                // scale too — `SELECT 1/3 UNION ALL SELECT 2.5 UNION ALL
                // SELECT 2` is 0.3333, 2.5000, 2.0000 there.
                //
                // The *values* and not the rendering, which is measurable
                // before anything is printed: `SELECT DISTINCT x FROM (SELECT
                // 1/3 AS x UNION ALL SELECT 0.3333) t` is one row on 8.3,
                // because the union writes the quotient into a four-place
                // field and the two stop being different numbers.
                //
                // Read off the rows rather than off a declared type, because
                // an expression's scale is not described here at all — that is
                // #0111 — so a literal branch has no column to take one from.
                // A branch that declares a scale and returns no row is what
                // that misses; a branch with rows carries its scale on each.
                for i in 0..left.columns.len() {
                    if !matches!(
                        left.columns[i].ty,
                        ColumnType::NewDecimal | ColumnType::Decimal
                    ) {
                        continue;
                    }
                    let widest = left
                        .rows
                        .iter()
                        .chain(right.rows.iter())
                        .filter_map(|row| row.get(i)?.display_scale())
                        .max()
                        .unwrap_or(0);
                    // Nothing to unify at no places at all, and saying so
                    // keeps a union of two integers wide enough to need a
                    // DECIMAL from being retagged one value at a time.
                    if widest == 0 {
                        continue;
                    }
                    for row in left.rows.iter_mut().chain(right.rows.iter_mut()) {
                        let Some(value) = row.get_mut(i) else {
                            continue;
                        };
                        let Some(exact) = value.exact_decimal() else {
                            continue;
                        };
                        if let Some(written) = crate::decimal::rescale_wide(&exact, widest) {
                            *value = Value::decimal(written);
                        }
                    }
                }

                let all = matches!(
                    set_quantifier,
                    SetQuantifier::All | SetQuantifier::AllByName
                );

                // With ALL, INTERSECT and EXCEPT are multiset operations: a row
                // appearing twice on the right cancels two rows on the left, not
                // all of them. Plain set membership gets those counts wrong.
                match op {
                    SetOperator::Union => left.rows.extend(right.rows),
                    SetOperator::Intersect => {
                        let mut available = row_counts(&right.rows);
                        left.rows
                            .retain(|row| match available.get_mut(&row_key(row)) {
                                Some(remaining) if *remaining > 0 => {
                                    if all {
                                        *remaining -= 1;
                                    }
                                    true
                                }
                                _ => false,
                            });
                    }
                    SetOperator::Except | SetOperator::Minus => {
                        let mut available = row_counts(&right.rows);
                        left.rows
                            .retain(|row| match available.get_mut(&row_key(row)) {
                                Some(remaining) if *remaining > 0 => {
                                    if all {
                                        *remaining -= 1;
                                    }
                                    false
                                }
                                _ => true,
                            });
                    }
                }

                if !all {
                    dedupe(&mut left.rows);
                }

                // A set operation materialises its result, and MySQL does not
                // carry an ENUM through one: measured on 8.3, the column of
                // `SELECT e FROM t UNION ALL SELECT e FROM t` is VAR_STRING
                // and `x + 0` over it is 0 — even with the *same* declaration
                // on both sides, so it is the materialising and not a mixture
                // that does it. Left tagged, the ordinals of two different
                // declarations would also have met in one sort key, where
                // position 1 means a different member on each side.
                for column in &mut left.columns {
                    if column.declared.as_ref().is_some_and(|d| {
                        matches!(
                            d.kind,
                            crate::types::TypeKind::Enum | crate::types::TypeKind::Set
                        )
                    }) {
                        column.ty = ColumnType::VarString;
                        column.declared = None;
                    }
                }
                // The sign goes too, wherever the column it lands in is not a
                // number. The temporary table has a field per column and a
                // field's UNSIGNED is an attribute of a numeric type, so an
                // expression that carried the flag while being a string does
                // not carry it out again: `SELECT X'41'` is UNSIGNED on 8.3
                // and `SELECT X'41' UNION SELECT X'42'` is not, though both
                // branches are. The same rule as `OutColumn::sql_type`, which
                // is where a derived table and a view lose it.
                //
                // Read after the ENUM correction above rather than before it,
                // because that one rewrites the type code, and this asks the
                // code.
                //
                // MySQL goes further than this and drops the flag for a
                // DECIMAL and a FLOAT too — `SELECT du FROM t UNION SELECT du
                // FROM t` over a `DECIMAL(10,2) UNSIGNED` is NEWDECIMAL with
                // no UNSIGNED on 8.3, where the integer beside it keeps it and
                // so does a YEAR. That is left alone deliberately: the loop
                // below reads this same field to decide whether to untag the
                // *values*, so widening the rule moves the arithmetic as well
                // as the description, and it is a divergence of its own rather
                // than part of this one.
                for column in &mut left.columns {
                    column.unsigned &= crate::result::is_numeric(column.ty);
                }
                // A boolean goes the same way, for the reason a derived
                // table takes it off too: the operation gives the value a
                // column to live in. Measured on 8.3, `SELECT JSON_ARRAY(x)
                // FROM (SELECT TRUE AS x UNION SELECT FALSE) t` is `[0]` and
                // `[1]`, never `[false]` and `[true]`.
                //
                // The UNSIGNED tag goes only where the merged column is
                // signed, which is not the same rule and so is decided per
                // column: measured on 8.3, `SELECT iu FROM t UNION SELECT iu
                // FROM t` keeps the flag and `x - 100` over it still raises
                // 1690, while the same union against a *signed* column drops
                // it and answers -93. `column.unsigned` above has already
                // taken the AND of the two branches, which is that rule.
                for row in &mut left.rows {
                    for (value, column) in row.iter_mut().zip(&left.columns) {
                        let merged = std::mem::replace(value, Value::Null)
                            .untag_enum()
                            .untag_bool();
                        *value = if column.unsigned {
                            merged
                        } else {
                            merged.untag_unsigned()
                        };
                    }
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
    ) -> EngineResult<ResultSet> {
        // A SELECT with no FROM evaluates its projection once, which is the
        // shape of `SELECT 1` and every driver's preamble query. One row
        // sorts the same whatever the ORDER BY says, but what it may *name*
        // is still only what the query returns: `SELECT 1 AS one ORDER BY 2`
        // is 1054 on 8.3, and this engine answered it.
        if select.from.is_empty() {
            let result = self.select_without_table(session, select, outer)?;
            check_projected_order(&result.columns, order, outer)?;
            return Ok(result);
        }

        // A JSON_TABLE in this FROM may take its document from the enclosing
        // query's row, so the outer row has to be in scope while the relation
        // is built — not only while its expressions are evaluated.
        let scoped;
        let session = match outer {
            Some(outer) => {
                scoped = {
                    let mut copy = session.clone();
                    copy.lateral = Some(outer.clone());
                    copy
                };
                &scoped
            }
            _ => session,
        };

        let relation = self.build_relation_list(session, &select.from)?;
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

        // The select list, before every other clause of the query body.
        //
        // 8.3 resolves it first and this engine resolved it late, which nothing
        // could see while every clause reported `'field list'` anyway: the
        // message was right about the clause whichever one had actually been
        // read. Naming the clauses correctly (#0099) made the order visible,
        // and it is the order, measured with a fault in two clauses at once:
        //
        //     SELECT zz.q FROM Cl WHERE yy.r = 1        'zz.q' in 'field list'
        //     SELECT zz.q FROM Cl GROUP BY yy.r         'zz.q' in 'field list'
        //     SELECT k FROM Cl WHERE zz.q = 1 GROUP BY yy.r
        //                                               'zz.q' in 'where clause'
        //
        // Everything the select list carries travels with it and not just its
        // names — `SELECT s LIKE 'a' ESCAPE '!!' FROM Cl WHERE nope = 1` is
        // 1210 on 8.3, `SELECT SIGN() FROM Cl WHERE nope = 1` is 1582, and
        // `SELECT nope.* FROM Cl WHERE zz.q = 1` is 1051 — which is why this is
        // the whole of `resolve_field_list` rather than a name check.
        //
        // A join's ON is the one clause still out of place: it is read while
        // the relation is being built, above, and this resolves against that
        // relation. 8.3 puts it after both the select list and the WHERE. That
        // ordering arrives with #0100, which owns settling an ON before its
        // first row.
        //
        // `project` runs this same pass again when it gets there; it is pure,
        // and one definition means a mutation to it neuters both.
        resolve_field_list(&select.projection, &refs, &relation)?;

        // Resolved before the loop, not inside it. A predicate is only
        // evaluated once per row, so over a table with no rows -- an empty
        // one, or any statement being described -- a name nothing answers to
        // was never looked up at all, and `SELECT b FROM t WHERE a.b = 1`
        // came back as an empty result where MySQL raises 1054.
        if let Some(predicate) = &select.selection {
            if let Some(name) = unresolved_column(predicate, &refs) {
                return Err(EngineError::unknown_column(&name, "where clause"));
            }
            // Same reason, one clause further in: an ESCAPE is settled per row
            // too, so `WHERE s LIKE 'a' ESCAPE '!!'` over an empty table came
            // back empty where 8.3 raises 1210.
            refuse_at_resolve([predicate])?;
        }

        let mut matching: Vec<&Vec<Value>> = Vec::new();
        for row in source {
            let ctx = Context::for_row(&refs, row, session).with_executor(self);
            let keep = match &select.selection {
                Some(predicate) => expr::eval(predicate, &ctx)?.truthy() == Some(true),
                None => true,
            };
            if keep {
                matching.push(row);
            }
        }

        // A WINDOW clause defines names the projection can reference. Ignoring
        // it would answer a different query than the one that was asked.
        if !select.named_window.is_empty() {
            return Err(unsupported("named windows"));
        }

        let group_exprs = match &select.group_by {
            GroupByExpr::Expressions(exprs, _) => exprs.clone(),
            GroupByExpr::All(_) => return Err(unsupported("GROUP BY ALL")),
        };

        // A query is grouped if it says so, or if it aggregates: an aggregate
        // with no GROUP BY is one group covering every row.
        // An aggregate anywhere in a select item groups the query, not just a
        // bare one: `SELECT SUM(a) / COUNT(*)` and `SELECT IFNULL(SUM(v), 0)`
        // are aggregate queries. So does one written in the HAVING and nowhere
        // else — `SELECT b FROM t HAVING COUNT(*) > 0` is 1140 on 8.3, the
        // complaint of an aggregated query with an ungrouped column in it.
        //
        // The *clause* does not group anything, which this read as meaning it
        // did: `SELECT b FROM t HAVING b > 1` answers the rows where b is
        // greater than 1 on 8.3, describing them exactly as `SELECT b FROM t`
        // does, and came back 1055 here — a working query refused. Without an
        // aggregate a HAVING is a second filter, settled per row after the
        // projection so it can name an output alias.
        //
        // An aggregate inside a *subquery* belongs to that subquery, which is
        // why this asks `contains_aggregate` — it does not descend into one —
        // rather than looking for the word: `HAVING b > (SELECT MIN(b) FROM t)`
        // is per-row on 8.3, and grouping it would collapse the answer to one
        // row.
        let aggregating = !group_exprs.is_empty() || aggregates_of_itself(select);

        if aggregating {
            let mut grouped = self.grouped(
                session,
                select,
                Input {
                    refs: &refs,
                    columns: &relation.columns,
                    matching: &matching,
                },
                &group_exprs,
                order,
            )?;
            apply_distinct(&mut grouped, select.distinct.as_ref())?;
            return Ok(grouped);
        }

        // A HAVING that groups nothing filters rows here, before anything is
        // projected — because a window function is computed over the rows the
        // HAVING kept, not over the rows the WHERE kept. Measured on 8.3:
        // `SELECT b, ROW_NUMBER() OVER (ORDER BY id) rn FROM Hv HAVING b > 1`
        // numbers the two surviving rows 1 and 2, and `COUNT(*) OVER ()`
        // beside it answers 2 rather than 4. Filtering after the projection
        // answered 2 and 3, and 4 — the window had already seen the rows the
        // clause was about to drop.
        //
        // Ahead of the sort too, which is also where 8.3 puts it: `SELECT b
        // FROM t HAVING nope > 1 ORDER BY alsonope` is 1054 for `nope` under
        // the HAVING, not for `alsonope`.
        self.filter_having(
            session,
            select,
            &group_exprs,
            &refs,
            relation.columns.len(),
            &mut matching,
        )?;

        // Sorting still happens before the projection, because ORDER BY may
        // name a column the select list does not return — `SELECT a FROM t
        // ORDER BY b` is legal on 8.3 — and because a window function is
        // computed over the rows in the order they are handed to it: `SELECT
        // id, ROW_NUMBER() OVER () rn FROM Hv ORDER BY id DESC` numbers the
        // rows 1..4 down the sorted order there. Projecting first to get the
        // select list would have renumbered them.
        //
        // What changes is what the key *is*. The clause is resolved against
        // the select list before any row is read, so a position means the
        // position in the select list and a bare name means the select list's
        // name, and only what neither answers falls through to the relation.
        //
        // Built only where there is a clause to resolve, because it copies the
        // select list and every SELECT comes through here.
        let items = match order {
            Some(_) => output_items(
                &select.projection,
                &relation,
                &refs,
                &coalesced_columns(&select.from),
            )?,
            None => Vec::new(),
        };
        // Not aggregated: an aggregate anywhere in the select list or the
        // HAVING sent the statement down `grouped` instead.
        let keys = resolve_order_by(order, &items, &refs, false, &|index| {
            items[index].expr.clone()
        })?;
        check_distinct_order(
            select.distinct.as_ref().and(order),
            &items,
            &refs,
            &relation,
        )?;
        let carried = if keys.is_empty() {
            None
        } else {
            // A key the source row cannot answer is a window function's value,
            // which only exists once the projection has computed it. What the
            // source rows *did* answer comes back here to be finished with it.
            self.sort_rows(session, &refs, &mut matching, &keys)?
        };

        let mut projected = self.project(session, select, &relation, &refs, &matching)?;
        // Finished here rather than by the caller, and before DISTINCT,
        // because `carried` lines up with these rows one for one — `project`
        // yields a row per matching row, in order — and a dedup would break
        // that alignment. Sorting first is what DISTINCT already assumes:
        // rows it collapses are equal, so which of them it keeps cannot
        // change the order, and every key is in the select list by then
        // because `check_distinct_order` refused the ones that are not.
        if let Some(carried) = carried {
            self.order_projected(&mut projected, session, order, &carried)?;
        }
        apply_distinct(&mut projected, select.distinct.as_ref())?;
        Ok(projected)
    }

    /// Applies a HAVING that groups nothing, dropping the rows it rejects.
    ///
    /// The ungrouped counterpart of the HAVING inside `grouped`, and it runs
    /// on the source rows rather than on projected ones so that everything
    /// downstream — the window functions above all — sees only what survived.
    ///
    /// An alias is reached by substituting the *expression* the select list
    /// wrote for it, where the grouped path binds the value it already
    /// computed. The two differ because nothing has been computed yet here,
    /// and it is sound for the same reason the grouped path is not: an alias a
    /// HAVING can name is never a window, since naming one is 3594, so the
    /// expression behind it is settled by this row alone.
    ///
    /// No aggregate can reach this — one written in the HAVING sends the
    /// statement down the grouped path instead.
    fn filter_having(
        &self,
        session: &Session,
        select: &Select,
        group_exprs: &[Expr],
        refs: &[ColumnRef],
        own: usize,
        matching: &mut Vec<&Vec<Value>>,
    ) -> EngineResult<()> {
        let Some(having) = &select.having else {
            return Ok(());
        };
        // After the field list, which the caller resolved. Before the ESCAPE
        // guard below, because 8.3 resolves a clause's names before its
        // arguments: `SELECT b FROM t HAVING zz LIKE 'a' ESCAPE '!!'` is 1054
        // there, not 1210.
        //
        // `group_exprs` is empty here by construction — a GROUP BY routes the
        // statement to `grouped` — and is passed rather than assumed so the
        // check has one contract wherever it is called from.
        check_having_scope(having, &select.projection, group_exprs, refs, own)?;
        refuse_at_resolve([having])?;

        let resolved = substitute_item_exprs(having, &select.projection);
        let mut keep = Vec::with_capacity(matching.len());
        for row in matching.iter() {
            let ctx = Context::for_row(refs, row, session).with_executor(self);
            keep.push(expr::eval(&resolved, &ctx)?.truthy() == Some(true));
        }
        *matching = std::mem::take(matching)
            .into_iter()
            .zip(keep)
            .filter_map(|(row, keep)| keep.then_some(row))
            .collect();
        Ok(())
    }

    /// Sorts source rows in place by keys already resolved against the select
    /// list.
    ///
    /// `None` when every key was answered and the rows are now in order.
    /// `Some(keys)` when at least one was not — one entry per row, in the
    /// order the rows arrived, carrying `None` for each key the source row
    /// cannot answer. The rows are left alone in that case, and the caller
    /// finishes the sort once the projection has computed what is missing.
    ///
    /// What is missing is a window function's value, or an alias for one: it
    /// exists only after the projection, and the projection numbers the rows
    /// in the order they reach it. So no key may be applied here when one is
    /// outstanding — sorting by the half that resolved would renumber the
    /// window. Measured on 8.3: `SELECT id, ROW_NUMBER() OVER () rn FROM t
    /// ORDER BY 2, b` numbers `rn` down the *table's* order and then sorts by
    /// `(rn, b)`.
    ///
    /// What resolved is carried rather than thrown away, which is the point of
    /// the shape: a key that did resolve may name a column the select list
    /// does not return, and re-deriving every key from the output made that
    /// same statement 1054 for `b`, under the field list's name at that.
    ///
    /// The clause's own checks — the names it writes, and its ESCAPEs — belong
    /// to `resolve_order_by` rather than here, because this loop turns any
    /// failure into "could not sort" and over no rows does not run at all: an
    /// error raised inside it is an error the empty case never reaches.
    fn sort_rows(
        &self,
        session: &Session,
        refs: &[ColumnRef],
        rows: &mut [&Vec<Value>],
        items: &[SortKey],
    ) -> EngineResult<Option<Vec<Vec<Option<Value>>>>> {
        // Per key rather than per row, so a key that failed once is not
        // evaluated again for every remaining row — and so the answer for it
        // is the same `None` on every row rather than a value on the rows
        // before the first failure.
        let mut outstanding = vec![false; items.len()];
        let mut keys: Vec<Vec<Option<Value>>> = Vec::with_capacity(rows.len());
        for row in rows.iter() {
            let ctx = Context::for_row(refs, row, session).with_executor(self);
            let mut key = Vec::with_capacity(items.len());
            for (position, item) in items.iter().enumerate() {
                if outstanding[position] {
                    key.push(None);
                    continue;
                }
                match expr::eval(&item.expr, &ctx) {
                    Ok(v) => key.push(Some(v)),
                    Err(_) => {
                        outstanding[position] = true;
                        key.push(None);
                    }
                }
            }
            keys.push(key);
        }

        if outstanding.iter().any(|o| *o) {
            for key in &mut keys {
                for (position, value) in key.iter_mut().enumerate() {
                    if outstanding[position] {
                        *value = None;
                    }
                }
            }
            return Ok(Some(keys));
        }

        let descending: Vec<bool> = items.iter().map(|i| i.descending).collect();
        let mut paired: Vec<(Vec<Value>, &Vec<Value>)> = keys
            .into_iter()
            .map(|key| key.into_iter().map(|v| v.unwrap_or(Value::Null)).collect())
            .zip(rows.iter().copied())
            .collect();
        paired.sort_by(|a, b| compare_keys(&a.0, &b.0, &descending));
        for (slot, (_, row)) in rows.iter_mut().zip(paired) {
            *slot = row;
        }
        Ok(None)
    }

    /// Narrows the rows a single-table UPDATE or DELETE touches, in the order
    /// it touches them.
    ///
    /// `matching` is every row the WHERE clause accepted, by index. ORDER BY
    /// and LIMIT then decide *which* of those the statement acts on, which is
    /// why neither may be dropped: `DELETE FROM t LIMIT 1` with the LIMIT
    /// ignored empties the table.
    ///
    /// The order matters even without a LIMIT, because it is the order the
    /// rows are written in — `UPDATE t SET id = id + 1 ORDER BY id DESC`
    /// succeeds where the same statement unordered collides with itself.
    fn dml_row_order(
        &self,
        session: &Session,
        refs: &[ColumnRef],
        rows: &[Vec<Value>],
        matching: Vec<usize>,
        order_by: &[OrderByExpr],
        limit: Option<&Expr>,
    ) -> EngineResult<Vec<usize>> {
        // Before a row is chosen, not while they are being sorted: the clause
        // was otherwise read once per matching row, so a WHERE that matched
        // nothing left it unread and `UPDATE Bare SET k = 1 WHERE id = 4242
        // ORDER BY zz.q LIMIT 1` reported nought rows affected where 8.3
        // refuses the statement outright. #0100.
        resolve_dml_order(order_by, refs)?;

        let mut chosen = matching;

        if !order_by.is_empty() {
            let mut keys: Vec<(Vec<Value>, usize)> = Vec::with_capacity(chosen.len());
            for index in chosen {
                // `'order clause'` here too, for a name only a row can turn
                // up: `resolve_dml_order` has settled everything fixed by the
                // text, and this evaluator still reports whatever is left.
                let ctx = Context::for_row(refs, &rows[index], session)
                    .with_executor(self)
                    .in_clause("order clause");
                let mut key = Vec::with_capacity(order_by.len());
                for item in order_by {
                    key.push(expr::eval(&item.expr, &ctx)?);
                }
                keys.push((key, index));
            }
            let descending: Vec<bool> = order_by
                .iter()
                .map(|i| i.options.asc == Some(false))
                .collect();
            keys.sort_by(|a, b| compare_keys(&a.0, &b.0, &descending));
            chosen = keys.into_iter().map(|(_, index)| index).collect();
        }

        if let Some(limit) = limit {
            let ctx = Context::rowless(session).with_executor(self);
            // An exact non-negative integer, not whatever coerces into one:
            // `as_int` truncates, so `LIMIT 1.9` would quietly become one row
            // and `LIMIT '2'` two. MySQL refuses both, and a LIMIT that is
            // rounded rather than refused is the wrong number of rows removed.
            let count = match expr::eval(limit, &ctx)? {
                Value::Int(count) if count >= 0 => count,
                _ => {
                    return Err(EngineError::parse(
                        "the LIMIT of an UPDATE or DELETE must be a non-negative row count",
                    ))
                }
            };
            chosen.truncate(count as usize);
        }

        Ok(chosen)
    }

    /// Materialises a comma-separated list of FROM or USING items.
    ///
    /// `FROM a, b` is a cross join, and `JSON_TABLE` is conventionally written
    /// that way — so the comma form is folded here rather than refused, with
    /// the same lateral handling a JOIN gets.
    fn build_relation_list(
        &self,
        session: &Session,
        items: &[TableWithJoins],
    ) -> EngineResult<Relation> {
        let Some((first, rest)) = items.split_first() else {
            return Err(unsupported("a FROM clause with no tables"));
        };
        let mut relation = self.build_relation(session, first)?;
        for item in rest {
            let cross = JoinOperator::CrossJoin(sqlparser::ast::JoinConstraint::None);
            relation = match &item.relation {
                // Built per row rather than once: evaluating it here would ask
                // for a column of the table on the left, which is not in scope
                // until there is a row to read it from.
                TableFactor::JsonTable { .. } if item.joins.is_empty() => {
                    self.lateral_join(session, relation, &item.relation, &cross)?
                }
                _ => {
                    let next = self.build_relation(session, item)?;
                    self.join(session, relation, next, &cross)?
                }
            };
        }
        Ok(relation)
    }

    /// Materialises a FROM clause, joining as needed.
    fn build_relation(&self, session: &Session, from: &TableWithJoins) -> EngineResult<Relation> {
        let mut left = self.relation_of(session, &from.relation)?;

        for join in &from.joins {
            // JSON_TABLE is implicitly LATERAL: its document is usually a
            // column of a table named earlier in the FROM, so it has to be
            // expanded once per row on the left rather than once for the join.
            if let TableFactor::JsonTable { .. } = &join.relation {
                left = self.lateral_join(session, left, &join.relation, &join.join_operator)?;
                continue;
            }

            let right = self.relation_of(session, &join.relation)?;
            left = self.join(session, left, right, &join.join_operator)?;
        }

        Ok(left)
    }

    /// Joins a factor that is re-evaluated for each row on the left.
    ///
    /// Each left row is joined to the rows its own evaluation produced, which
    /// is what makes `JSON_TABLE(t.doc, ...)` mean the document in *that* row.
    fn lateral_join(
        &self,
        session: &Session,
        left: Relation,
        factor: &TableFactor,
        operator: &JoinOperator,
    ) -> EngineResult<Relation> {
        use sqlparser::ast::JoinConstraint;

        // The same set `join()` accepts, refused the same way: a RIGHT or FULL
        // join here would otherwise quietly behave as an inner one.
        let (constraint, outer_left) = match operator {
            JoinOperator::Inner(c) | JoinOperator::Join(c) => (Some(c), false),
            JoinOperator::LeftOuter(c) | JoinOperator::Left(c) => (Some(c), true),
            JoinOperator::CrossJoin(_) => (None, false),
            other => return Err(unsupported(format!("join type `{other:?}`"))),
        };

        // Before the loop over the left rows, for the reason an ordinary
        // join's is before its own: an empty left side expands nothing, so a
        // clause only read per produced row is never read at all.
        if let Some(JoinConstraint::On(predicate)) = constraint {
            refuse_at_resolve([predicate])?;
        }

        let refs = left.refs();
        let mut columns: Option<Vec<crate::relation::RelColumn>> = None;
        let mut rows = Vec::new();
        let mut origins = Vec::new();

        for (index, row) in left.rows.iter().enumerate() {
            // The left row is put in scope as an outer row, which is the same
            // seam a correlated subquery uses. Any enclosing correlation is
            // kept behind it, so a subquery that names another table first can
            // still reach the row it is correlated to.
            let mut scoped = session.clone();
            let mut schema = refs.clone();
            let mut values = row.clone();
            if let Some(enclosing) = &session.lateral {
                schema.extend(enclosing.schema.iter().cloned());
                values.extend(enclosing.values.iter().cloned());
            }
            scoped.lateral = Some(OuterRow { schema, values });
            let expanded = self.relation_of(&scoped, factor)?;

            // The shape does not depend on the row, so the first one settles it
            // — and an empty left side still has to produce columns.
            let width = expanded.columns.len();
            if columns.is_none() {
                columns = Some(expanded.columns.clone());
            }

            // The ON clause is evaluated against the joined row, exactly as it
            // is for an ordinary join. Ignoring it would turn every
            // `JOIN JSON_TABLE(...) ON ...` into a cross join.
            //
            // The enclosing query's columns are appended to the scope for the
            // same reason `join` appends them: a `JOIN JSON_TABLE(...) ON` in
            // a correlated subquery may name the row the subquery is being
            // run for. `session` and not `scoped` — `scoped` carries the row
            // on the *left* of this join, which the joined row already has.
            let mut joined_refs = refs.clone();
            joined_refs.extend(
                expanded
                    .columns
                    .iter()
                    .map(crate::relation::RelColumn::reference),
            );
            let (outer_schema, outer_values) = correlated_scope(session);
            joined_refs.extend(outer_schema.iter().cloned());

            let mut matched = false;
            for produced in expanded.rows {
                let mut joined = row.clone();
                joined.extend(produced);

                let keep = match constraint {
                    None | Some(JoinConstraint::None) => true,
                    Some(JoinConstraint::On(predicate)) => {
                        // The lateral half of the pair below, named the same
                        // way and for the same reason — including the
                        // enclosing row read beside the joined one.
                        let values = with_outer(&joined, outer_values);
                        let ctx = Context::for_row(&joined_refs, &values, session)
                            .with_executor(self)
                            .in_clause("on clause");
                        expr::eval(predicate, &ctx)?.truthy() == Some(true)
                    }
                    Some(other) => {
                        return Err(unsupported(format!("`{other:?}` on a lateral join")))
                    }
                };
                if !keep {
                    continue;
                }

                matched = true;
                rows.push(joined);
                origins.push(left.origins[index].clone());
            }

            // A left join keeps the row even when nothing on the right matched.
            if !matched && outer_left {
                let mut padded = row.clone();
                padded.extend(std::iter::repeat_n(Value::Null, width));
                rows.push(padded);
                origins.push(left.origins[index].clone());
            }
        }

        // With no rows on the left the factor was never evaluated, so the
        // shape is worked out once against a row of NULLs.
        //
        // Not against nothing: `JSON_TABLE(t.doc, ...)` is correlated, so
        // evaluating it with no row in scope fails to resolve `t.doc` and the
        // whole relation errors. Its shape is its COLUMNS clause and not its
        // document, so a NULL document gives the same columns and no rows —
        // which is what describing a prepared statement over an empty table
        // needs, and what an empty table needs at execution too.
        let right_columns = match columns {
            Some(columns) => columns,
            None => {
                let mut scoped = session.clone();
                let mut schema = refs.clone();
                let mut values = vec![Value::Null; schema.len()];
                if let Some(enclosing) = &session.lateral {
                    schema.extend(enclosing.schema.iter().cloned());
                    values.extend(enclosing.values.iter().cloned());
                }
                scoped.lateral = Some(OuterRow { schema, values });
                self.relation_of(&scoped, factor)?.columns
            }
        };

        let mut all = left.columns;
        all.extend(right_columns);
        Ok(Relation {
            columns: all,
            rows,
            sources: left.sources,
            base_tables: left.base_tables,
            source_databases: left.source_databases,
            origins,
        })
    }

    /// Nested-loop join of two materialised relations.
    ///
    /// `session.lateral` is read for the ON, so a join inside a correlated
    /// subquery can name the row the subquery is being run for. See
    /// [`correlated_scope`].
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

        // A LEFT JOIN pads the right side and a RIGHT JOIN the left, so each
        // relaxes the one the other keeps.
        let columns = left.joined_columns(&right, outer_right, outer_left);
        let own_refs: Vec<ColumnRef> = columns
            .iter()
            .map(crate::relation::RelColumn::reference)
            .collect();

        let condition = match constraint {
            Some(JoinConstraint::On(e)) => Some(e.clone()),
            Some(JoinConstraint::None) | None => None,
            // USING(a, b) is equality on each named column.
            Some(JoinConstraint::Using(names)) => Some(using_condition(names, &left, &right)?),
            Some(other) => return Err(unsupported(format!("join constraint `{other:?}`"))),
        };

        // Settled before the loops, because the condition is only otherwise
        // read once per pair of rows: a join where either side is empty
        // produces no pair, so `ON a.s LIKE b.s ESCAPE '!!'` answered no rows
        // where 8.3 raises 1210, and `ON zz.q = Cr.id` answered an empty set
        // where it raises 1054. That second one is #0100's half — the names,
        // which are as fixed by the text as an ESCAPE is and were the one
        // thing here still waiting for a row.
        //
        // `'on clause'`, measured, and the name a `USING` list's rewritten
        // equality carries too — its own names are `'from clause'` and are
        // resolved ahead of this, in `using_condition`. A clause built there
        // rather than written has no ESCAPE and no unknown name to find.
        //
        // The name is asked *after* the arguments, which is the other way
        // round from every other clause here, and deliberately. 8.3 reads an
        // ON left to right and answers whichever fault comes first — measured,
        // all four ways:
        //
        //     ON nope = 1 AND s LIKE 'a' ESCAPE '!!'   1054
        //     ON s LIKE 'a' ESCAPE '!!' AND nope = 1   1210
        //     ON nope = 1 AND SIGN() = 1               1582
        //     ON SIGN() = 1 AND nope = 1               1582
        //
        // A single expression cannot be walked positionally here: names are
        // one walk and arguments are another, and merging them is #0087's.
        // So one of the two orders has to be wrong, and arguments first — the
        // order that was already here — is right for three of those four.
        refuse_at_resolve(condition.as_ref())?;

        // The enclosing query's columns as well as this join's, or an ON that
        // correlates outwards is refused for naming something that *is* in
        // scope: `SELECT k FROM A WHERE EXISTS (SELECT 1 FROM B JOIN C ON C.id
        // = A.id)` answers on 8.3, and reading only this join's columns turned
        // it into a 1054. `session.lateral` is where `select` puts the outer
        // row while a relation is being built.
        //
        // The same scope for the resolution below and for every row's
        // evaluation, which is #0230: #0100 widened the resolution alone, so
        // the clause stopped refusing a correlated ON before the first row and
        // then raised the identical 1054 on reaching one — a refusal moved
        // rather than removed, and only visible once the inner side had rows
        // to reach. Appended rather than prepended, so a name this join
        // answers to wins over the enclosing query's, which is MySQL's scoping
        // rule and the one `select` already applies to its own clauses.
        let (outer_schema, outer_values) = correlated_scope(session);
        let mut scope = own_refs;
        scope.extend(outer_schema.iter().cloned());
        if let Some(e) = &condition {
            if let Some(name) = unresolved_column(e, &scope) {
                return Err(EngineError::unknown_column(&name, "on clause"));
            }
        }

        let null_right = vec![Value::Null; right.columns.len()];
        let null_left = vec![Value::Null; left.columns.len()];
        let unmatched_right: Vec<Option<usize>> = vec![None; right.sources.len()];
        let unmatched_left: Vec<Option<usize>> = vec![None; left.sources.len()];
        let mut rows = Vec::new();
        let mut origins: Vec<Vec<Option<usize>>> = Vec::new();
        let mut right_matched = vec![false; right.rows.len()];

        for (left_index, left_row) in left.rows.iter().enumerate() {
            let mut matched = false;
            for (i, right_row) in right.rows.iter().enumerate() {
                let mut combined = left_row.clone();
                combined.extend_from_slice(right_row);

                let keep = match &condition {
                    Some(e) => {
                        // `'on clause'`, which is 8.3's name for a join
                        // condition whichever join wrote it — INNER, LEFT and
                        // the `USING` list's rewritten equality all report it.
                        // The `USING` *names* themselves are `'from clause'`
                        // and are resolved before this, in `using_condition`,
                        // which is 8.3's order too.
                        //
                        // What is *not* 8.3's order is what comes between: the
                        // select list and the WHERE both beat an ON there, and
                        // both come after it here, because an ON is read while
                        // the relation is being built and the other two resolve
                        // against the relation. Measured: `SELECT zz.q FROM Cl
                        // JOIN Cr ON yy.r = Cr.id` is `'zz.q' in 'field list'`
                        // on 8.3 and `'yy.r'` here — the wrong name in either
                        // spelling of the clause, and the fix is to settle an
                        // ON before its first row, which is #0100's.
                        //
                        // The enclosing row is read beside the pair, and only
                        // for the evaluation: what the join *produces* is its
                        // own two sides and nothing else, so the correlation
                        // is never appended to the row that gets kept.
                        let values = with_outer(&combined, outer_values);
                        let ctx = Context::for_row(&scope, &values, session)
                            .with_executor(self)
                            .in_clause("on clause");
                        expr::eval(e, &ctx)?.truthy() == Some(true)
                    }
                    None => true,
                };

                if keep {
                    matched = true;
                    right_matched[i] = true;
                    rows.push(combined);
                    let mut origin = left.origins[left_index].clone();
                    origin.extend(right.origins[i].iter().copied());
                    origins.push(origin);
                }
            }
            // A LEFT JOIN keeps the left row even with no match, padded right.
            if outer_left && !matched {
                let mut combined = left_row.clone();
                combined.extend_from_slice(&null_right);
                rows.push(combined);
                let mut origin = left.origins[left_index].clone();
                origin.extend(unmatched_right.iter().copied());
                origins.push(origin);
            }
        }

        if outer_right {
            for (i, right_row) in right.rows.iter().enumerate() {
                if !right_matched[i] {
                    let mut combined = null_left.clone();
                    combined.extend_from_slice(right_row);
                    rows.push(combined);
                    let mut origin = unmatched_left.clone();
                    origin.extend(right.origins[i].iter().copied());
                    origins.push(origin);
                }
            }
        }

        let mut sources = left.sources.clone();
        sources.extend(right.sources.iter().cloned());
        let mut base_tables = left.base_tables.clone();
        base_tables.extend(right.base_tables.iter().cloned());
        let mut source_databases = left.source_databases.clone();
        source_databases.extend(right.source_databases.iter().cloned());
        Ok(Relation {
            columns,
            rows,
            sources,
            base_tables,
            source_databases,
            origins,
        })
    }

    /// Materialises one table factor.
    fn relation_of(&self, session: &Session, factor: &TableFactor) -> EngineResult<Relation> {
        match factor {
            TableFactor::Table { name, alias, .. } => {
                // A CTE shadows a table of the same name, and is looked up
                // before the catalog for exactly that reason. Innermost wins.
                //
                // The *same* name, spelled the same way: a CTE name is a table
                // name, and the MySQL this engine is compared against runs on
                // Linux with `lower_case_table_names = 0`. Measured on 8.3,
                // `WITH q AS (SELECT 1) SELECT * FROM Q` is 1146 and so is the
                // pair the other way round. Matched case-insensitively, a
                // lowercase CTE swallowed the PascalCase table it was reading
                // — `WITH RECURSIVE folder AS (... FROM Folder ...)` joined
                // itself instead of the table, which is the shape #0192 is
                // about. Only the CTE *name* is exact; a column qualifier is a
                // separate question and is left alone.
                let unqualified = name.0.len() == 1;
                let plain = name::dotted(name);
                if unqualified {
                    if let Some((_, cte)) = session.ctes.iter().rev().find(|(n, _)| *n == plain) {
                        let shown = alias
                            .as_ref()
                            .map(|a| a.name.value.clone())
                            .unwrap_or_else(|| plain.clone());
                        let mut relation = cte.clone();
                        for column in &mut relation.columns {
                            column.table = shown.clone();
                        }
                        return Ok(relation);
                    }
                }

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

                // An alias replaces the table name for qualified references,
                // but a write still has to find the table it came from — and a
                // self-join is only expressible with aliases, so both are kept.
                let shown = alias
                    .as_ref()
                    .map(|a| a.name.value.clone())
                    .unwrap_or_else(|| table_name.clone());
                // An alias also takes the item *out* of its database as far
                // as the *name* of a DELETE target is concerned, which is the
                // opposite of what it does to a column reference.
                // `SourceDatabase` has the statements that settle it.
                let addressable = alias.is_none();

                // Describing a statement reads the shape and leaves the rows
                // behind. The table still has to be found, so preparing a
                // statement that names one that does not exist fails the same
                // way running it would.
                let with_rows = !SHAPING.with(std::cell::Cell::get);
                if db.eq_ignore_ascii_case(INFORMATION_SCHEMA) {
                    let catalog = self.catalog()?;
                    // Built empty rather than built and discarded: describing
                    // `SELECT ... FROM information_schema.columns` against the
                    // target schema would otherwise assemble a row per column
                    // of every table, on every prepare.
                    let table = if with_rows {
                        info_schema::table(&catalog, session, &table_name)?
                    } else {
                        info_schema::shape(session, &table_name)?
                    };
                    return Ok(Relation::from_table_named(
                        &db,
                        addressable,
                        &table,
                        &shown,
                        with_rows,
                    ));
                }
                let catalog = self.catalog()?;
                let stored = catalog
                    .database(&db)
                    .ok_or_else(|| EngineError::unknown_database(&db))?
                    .table(&table_name)
                    .ok_or_else(|| EngineError::no_such_table(&table_name))?;
                Ok(Relation::from_table_named(
                    &db,
                    addressable,
                    stored,
                    &shown,
                    with_rows,
                ))
            }
            // `JSON_TABLE(doc, '$[*]' COLUMNS(...))` turns a document into
            // rows. The migrations use it to unpack a stored configuration
            // array into something they can join against.
            TableFactor::JsonTable {
                json_expr,
                json_path,
                columns,
                alias,
            } => self.json_table(session, json_expr, json_path, columns, alias.as_ref()),

            TableFactor::Derived {
                subquery, alias, ..
            } => {
                let inner = self.query(session, subquery)?;
                // Whether the columns take the shape a temporary table would
                // give them, or stay the expressions MySQL would have merged
                // into this query. See `materialised_by_mysql`.
                let narrowed = materialised_by_mysql(subquery);
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
                            // A derived table is in no database, so 8.3
                            // compares the qualifier against nothing: `SELECT
                            // nodb.d.b FROM (SELECT b FROM J2) d` answers
                            // there. `SELECT probe.d.*` over the same item is
                            // 1051, which `has_source` is where that lives.
                            qualifier_database: String::new(),
                            table: name.clone(),
                            name: c.name.clone(),
                            ty: if narrowed {
                                c.materialised_sql_type()
                            } else {
                                c.sql_type()
                            },
                            nullable: c.nullable,
                        })
                        .collect(),
                    // A derived table has no base row to write back to.
                    origins: vec![Vec::new(); inner.rows.len()],
                    rows: materialised(inner.rows),
                    sources: Vec::new(),
                    base_tables: Vec::new(),
                    source_databases: Vec::new(),
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
            Some(outer) => Context::for_row(&outer.schema, &outer.values, session),
        }
        .with_executor(self);
        let mut columns = Vec::new();
        let mut row = Vec::new();

        // A FROM-less SELECT is evaluated against *one* implicit row, not none:
        // `SELECT COUNT(*)` is 1 in MySQL, `SELECT SUM(1)` is 1 and
        // `SELECT MAX(2)` is 2. Folding over an empty group would answer
        // `0, NULL, NULL` — which reads as a result rather than as a mistake —
        // and would never evaluate the aggregate's argument at all.
        //
        // The row is the enclosing one where there is one, so a correlated
        // `SELECT (SELECT COUNT(*)) FROM t` still counts its own row.
        let (implicit_schema, implicit_values) = match outer {
            Some(outer) => (outer.schema.clone(), outer.values.clone()),
            None => (Vec::new(), Vec::new()),
        };
        let implicit_row = [&implicit_values];

        let items: Vec<(String, &Expr)> = select
            .projection
            .iter()
            .map(|item| match item {
                SelectItem::UnnamedExpr(e) => Ok((render_expr(e), e)),
                SelectItem::ExprWithAlias { expr, alias } => Ok((name::alias(alias), expr)),
                other => Err(unsupported(format!("select item `{other}`"))),
            })
            .collect::<EngineResult<_>>()?;

        // The field list is resolved before anything is evaluated, and before
        // the WHERE below, because that is the order 8.3 answers in: `SELECT
        // nope WHERE zz = 1` is 1054 for `nope` in the field list, not for
        // `zz` in the where clause.
        //
        // The same helper the table-backed path resolves its field list with,
        // so a check added to either reaches both.
        for (_, e) in &items {
            resolve_projected_expr(e, &implicit_schema)?;
        }

        // The WHERE, which this path used to drop on the floor entirely:
        // `SELECT 1 WHERE 0` and `SELECT 1 WHERE 1 = 0` each answered a row
        // where 8.3 answers none, and nothing written in the clause was ever
        // reached — `SELECT 1 WHERE 1 IN (SELECT SIGN(1,2))` answered a row
        // rather than 1582, and a malformed ESCAPE in it went the same way.
        //
        // Names first and the arguments after, which is what the table-backed
        // path does and the same helpers, so a mutation to either neuters both
        // paths at once.
        let kept = match &select.selection {
            None => true,
            Some(predicate) => {
                if let Some(name) = unresolved_column(predicate, &implicit_schema) {
                    return Err(EngineError::unknown_column(&name, "where clause"));
                }
                refuse_at_resolve([predicate])?;
                expr::eval(predicate, &ctx)?.truthy() == Some(true)
            }
        };

        // A rejected implicit row leaves *no* rows to aggregate over rather
        // than none to return, which is the whole of the difference between
        // `SELECT 1 WHERE 0` and `SELECT COUNT(*) WHERE 0`: measured on 8.3
        // the first is empty and the second answers one row holding 0, with
        // `SUM`, `MAX`, `MIN`, `AVG` and `GROUP_CONCAT` all NULL beside it.
        // An ungrouped aggregate always answers exactly one row, and over an
        // empty relation that row is the fold over nothing.
        //
        // Its argument is then never evaluated, which is 8.3's behaviour and
        // not a shortcut: measured there, `SELECT SUM(9223372036854775807 + 1)
        // WHERE 0` answers NULL and the same statement with `WHERE 1` is 1690.
        // What the argument is still held to is what 8.3 settles while
        // resolving — the field list pass above raises 1210 for `SUM('a' LIKE
        // 'b' ESCAPE '!!') WHERE 0` exactly as 8.3 does. The gap left is a
        // function 8.3 resolves by *name*: `SUM(NOSUCHFN(1)) WHERE 0` is 1305
        // there and NULL here, because this engine learns a function does not
        // exist by trying to call it. That is the table-backed path's answer
        // over an empty table too, and closing it means a catalog of names
        // rather than anything about this clause.
        let group: &[&Vec<Value>] = if kept { &implicit_row } else { &[] };

        let group_exprs: &[Expr] = match &select.group_by {
            GroupByExpr::Expressions(exprs, _) => exprs,
            GroupByExpr::All(_) => return Err(unsupported("GROUP BY ALL")),
        };

        // The GROUP BY was dropped on the floor here too. It can never change
        // *which* rows come back — one implicit row is one group whatever it
        // is grouped on — so the whole of what it decides is what it refuses,
        // and every refusal was missing: `SELECT 1 GROUP BY 2` and `SELECT 1
        // GROUP BY zz` are both 1054 in 'group statement' on 8.3 and answered
        // a row here, and `GROUP BY 'x' LIKE 'a' ESCAPE '!!'` is 1210 there
        // and answered one too.
        //
        // After the WHERE and before the HAVING, which is 8.3's order:
        // `SELECT 1 WHERE zz = 1 GROUP BY 2` is 1054 for `zz` and `SELECT 1
        // GROUP BY 2 HAVING zz > 1` is 1054 for the position.
        //
        // The clause reads the select list's names as well as the relation's,
        // so `SELECT 1 AS a GROUP BY a` answers rather than raising, and
        // `SELECT COUNT(*) AS n GROUP BY n` is the 1056 8.3 raises for it.
        //
        // The resolved list is dropped: one implicit row is one group whatever
        // it is grouped on, so this clause can only ever refuse here. Resolved
        // all the same, because a refusal it reaches only after substituting
        // an alias is a refusal the two paths have to share.
        if !group_exprs.is_empty() {
            resolve_group_by(group_exprs, &select.projection, &items, &implicit_schema)?;
        }

        // Every item is evaluated even where the WHERE has already discarded
        // the row, because a column's *definition* is not the only thing this
        // loop settles: an item this engine cannot compute is only found out
        // by computing it, so skipping the work would turn `SELECT NOSUCHFN(1)
        // WHERE 0` from a refusal into an answer. The definitions themselves
        // are read off the expression rather than off the value, so nothing
        // the client is sent depends on the evaluation succeeding.
        for (alias, e) in &items {
            let value = if let Expr::Function(f) = e {
                if f.over.is_none() && is_aggregate(&f.name.to_string()) {
                    let name = f.name.to_string().to_ascii_lowercase();
                    self.fold(
                        session,
                        &name,
                        &AggregateCall::read(f)?,
                        &implicit_schema,
                        group,
                    )?
                } else {
                    expr::eval(e, &ctx)?
                }
            } else {
                expr::eval(e, &ctx)?
            };
            // A literal knows whether it can be NULL; anything computed does
            // not, and says so. The distinction reaches a union, which takes
            // the nullability of either branch — so `SELECT id FROM t UNION
            // SELECT 1` must not be told the `1` might be missing.
            // From the expression, not from the value it happened to produce:
            // a later row could produce another type, and describing a
            // statement binds its parameters to NULL so there is often no
            // value worth reading.
            // `implicit_schema` rather than nothing, so a scalar subquery in a
            // FROM-less select still resolves against the enclosing row it is
            // evaluated against: `SELECT (SELECT (SELECT t.b)) FROM t` reads
            // `t.b` through two of these.
            let sub = Some(Sub {
                engine: self,
                session,
            });
            let mut column = OutColumn::new(
                alias.clone(),
                computed_column_type(e, &implicit_schema, &[], sub),
            );
            column.nullable = !never_null(e, &implicit_schema, &[], sub);
            column.unsigned = unsigned_answer(e, &implicit_schema, &[], sub);
            column.binary_values = binary_answer(e, &implicit_schema, sub);
            let shape = item_shape(e, &implicit_schema, &[], sub);
            column.max_length = shape.max_length;
            column.decimals = shape.decimals;
            columns.push(column);
            row.push(value);
        }

        // A FROM-less SELECT still has a HAVING, and this path used to drop
        // the clause on the floor: `SELECT 1 HAVING 0` answered a row on this
        // engine and answers none on 8.3, and `SELECT 1 HAVING zz > 0`
        // answered a row where 8.3 is 1054. The one implicit row is the group,
        // so an aggregate written in the clause folds over it exactly as one
        // in the select list already does — `SELECT COUNT(*) HAVING COUNT(*) >
        // 5` is empty on both.
        //
        // A row survives a WHERE that rejected the implicit one only where the
        // query aggregates without grouping, because that is the one shape
        // whose answer does not depend on there being a row: `SELECT COUNT(*)
        // WHERE 0` is one row on 8.3 and `SELECT 1 WHERE 0` is none. An
        // aggregate written only in the HAVING counts, exactly as it does on
        // the table-backed path — `SELECT 1 WHERE 0 HAVING COUNT(*) = 0`
        // answers a row there. A GROUP BY takes it away again: no rows means
        // no groups, so `SELECT COUNT(*) WHERE 0 GROUP BY 'a'` is empty.
        let mut rows = if kept || (aggregates_of_itself(select) && group_exprs.is_empty()) {
            vec![row]
        } else {
            Vec::new()
        };
        if let Some(having) = &select.having {
            // The clause is resolved whether or not there is a row left for it
            // to filter, for the reason the WHERE above is: `SELECT 1 WHERE 0
            // HAVING zz > 1` is 1054 in 'having clause' on 8.3, not an empty
            // set.
            //
            // No GROUP BY columns are passed even where the clause has some,
            // because on this path they can only be a constant or a select
            // item's own name — the scope already reaches both, and passing an
            // unresolved position would widen it by the literal rather than by
            // the column it names.
            check_having_scope(having, &select.projection, &[], &implicit_schema, 0)?;
            refuse_at_resolve([having])?;
            if let Some(projected) = rows.first() {
                let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
                let resolved = substitute_aliases(having, &names);
                // Over `group` rather than the implicit row, so an aggregate
                // in the clause folds over what the WHERE left exactly as one
                // in the select list does.
                let value =
                    self.eval_over_group(&resolved, group, &implicit_schema, session, projected)?;
                if value.truthy() != Some(true) {
                    rows.clear();
                }
            }
        }

        Ok(ResultSet { columns, rows })
    }

    fn project(
        &self,
        session: &Session,
        select: &Select,
        relation: &Relation,
        refs: &[ColumnRef],
        matching: &[&Vec<Value>],
    ) -> EngineResult<ResultSet> {
        let sub = Some(Sub {
            engine: self,
            session,
        });
        let mut columns: Vec<OutColumn> = Vec::new();
        let mut plan: Vec<Projection> = Vec::new();
        // A window function sees the whole result rather than one row, so its
        // values are computed up front and read back by row index.
        let mut windows: Vec<Vec<Value>> = Vec::new();

        let push_all = |columns: &mut Vec<OutColumn>,
                        plan: &mut Vec<Projection>,
                        source: Option<(Option<&str>, &str)>| {
            for (i, column) in relation.columns.iter().enumerate() {
                if source
                    .is_some_and(|(database, table)| !column.reference().in_source(database, table))
                {
                    continue;
                }
                columns.push(
                    OutColumn::from_sql_type(&column.name, &column.ty, column.nullable)
                        .in_table(&column.schema, &column.table)
                        .projected_from(&column.name, base_table(relation, &column.table)),
                );
                plan.push(Projection::Column(i));
            }
        };

        // Every name and every ESCAPE in the list, before any of it is read for
        // anything else. Hoisted out of the loop below so the HAVING can be
        // settled between this and the window computation, which is where 8.3
        // settles it — see `filter_having`.
        resolve_field_list(&select.projection, refs, relation)?;

        for item in &select.projection {
            match item {
                SelectItem::Wildcard(_) => push_all(&mut columns, &mut plan, None),
                SelectItem::QualifiedWildcard(kind, _) => {
                    // `resolve_field_list` above has already refused a
                    // qualifier no FROM item answers to, database included.
                    let (database, table) = wildcard_qualifier(kind)
                        .ok_or_else(|| EngineError::bad_table(&wildcard_written(kind)))?;
                    push_all(&mut columns, &mut plan, Some((database.as_deref(), &table)));
                }
                SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                    let alias = match item {
                        SelectItem::ExprWithAlias { alias, .. } => Some(name::alias(alias)),
                        _ => None,
                    };

                    if let Expr::Function(f) = e {
                        if f.over.is_some() {
                            let values = self.compute_window(f, refs, matching, session)?;
                            // Typed from the function, not the first value: a
                            // later row may differ, and the binary protocol
                            // encodes by the declared type.
                            let mut column = OutColumn::new(
                                alias.unwrap_or_else(|| render_expr(e)),
                                computed_column_type(e, refs, &relation.columns, sub),
                            );
                            // A ranking window numbers its rows and always has
                            // an answer; one that reaches past the frame has
                            // one only if it was given a default.
                            column.nullable = !never_null(e, refs, &relation.columns, sub);
                            column.unsigned = unsigned_answer(e, refs, &relation.columns, sub);
                            column.binary_values = binary_answer(e, refs, sub);
                            let shape = item_shape(e, refs, &relation.columns, sub);
                            column.max_length = shape.max_length;
                            column.decimals = shape.decimals;
                            columns.push(column);
                            plan.push(Projection::Window(windows.len()));
                            windows.push(values);
                            continue;
                        }
                    }

                    let (column, projection) = self.plan_expr(e, alias, relation, refs, session)?;
                    columns.push(column);
                    plan.push(projection);
                }
                other => return Err(unsupported(format!("select item `{other}`"))),
            }
        }

        let mut rows = Vec::with_capacity(matching.len());
        for (index, row) in matching.iter().enumerate() {
            let ctx = Context::for_row(refs, row, session).with_executor(self);
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

        // The OVER spec's two clauses, before the first partition rather than
        // per row. `unresolved_column` walks into a call's arguments and stops
        // at its OVER spec, deliberately: it carries one clause name for the
        // whole expression and these two have names of their own, so the field
        // list's pass over the projection cannot report them. That left them
        // settled only by the evaluator, once per row — so over an empty table
        // `SUM(k) OVER (PARTITION BY zz.q)` answered an empty set where 8.3
        // raises 1054, and a prepared statement was accepted that MySQL
        // refuses. #0100.
        //
        // `'window partition by'` and `'window order by'`, measured, and
        // PARTITION BY first because that is the order 8.3 reads them in.
        //
        // Names only. What an OVER spec carries besides them is already
        // settled, and settled earlier: `resolve_field_list` runs
        // `refuse_at_resolve` over the whole projection item and
        // `function_carried` descends into the spec, so a bad ESCAPE or a
        // wrong arity in one is refused before this is reached at all. That
        // puts the arguments ahead of the names here where 8.3 reads the list
        // left to right — `PARTITION BY nope, s LIKE 'a' ESCAPE '!!'` is 1054
        // there and 1210 here, and the other order agrees at 1210 — which is
        // the same within-clause ordering gap #0087 owns for the WHERE, not
        // something this site can settle on its own.
        //
        // `refs` and nothing else: an OVER spec does *not* see the select
        // list's aliases, measured — `SELECT k AS a, SUM(id) OVER (PARTITION
        // BY a) FROM Cl` is 1054 for `a` on 8.3, where the same alias in the
        // query's own ORDER BY resolves.
        if let Some(name) = spec
            .partition_by
            .iter()
            .find_map(|e| unresolved_column(e, refs))
        {
            return Err(EngineError::unknown_column(&name, "window partition by"));
        }
        if let Some(name) = spec
            .order_by
            .iter()
            .find_map(|item| unresolved_column(&item.expr, refs))
        {
            return Err(EngineError::unknown_column(&name, "window order by"));
        }

        let name = f.name.to_string().to_ascii_lowercase();
        let call = AggregateCall::read(f)?;

        // Takes the clause rather than fixing one, because the three things
        // read through here are written in three different clauses: the OVER
        // spec's PARTITION BY, its ORDER BY, and the call's own argument,
        // which belongs to whatever clause the *call* is written in. Measured
        // on 8.3, and the reason `unresolved_column` leaves an OVER spec alone
        // rather than reporting it under the field list's name.
        let evaluate = |e: &Expr, row: &Vec<Value>, clause: &'static str| -> EngineResult<Value> {
            let ctx = Context::for_row(refs, row, session)
                .with_executor(self)
                .in_clause(clause);
            expr::eval(e, &ctx)
        };

        // Partition, keeping each row's original index so results land back
        // where the projection will look for them.
        let mut partition_keys: Vec<Vec<String>> = Vec::new();
        let mut partitions: Vec<Vec<usize>> = Vec::new();
        for (index, row) in matching.iter().enumerate() {
            let mut key = Vec::with_capacity(spec.partition_by.len());
            for e in &spec.partition_by {
                let value = evaluate(e, row, "window partition by")?;
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
                    key.push(evaluate(&item.expr, matching[*index], "window order by")?);
                }
                ordered.push((key, *index));
            }
            ordered.sort_by(|a, b| compare_keys(&a.0, &b.0, &descending));

            // The default frame is RANGE BETWEEN UNBOUNDED PRECEDING AND
            // CURRENT ROW, and in RANGE mode "current row" means the last row
            // of the current row's peer group — every row with an equal ORDER
            // BY key. Without ORDER BY the frame is the whole partition.
            let frame_end: Vec<usize> = if spec.order_by.is_empty() {
                vec![ordered.len().saturating_sub(1); ordered.len()]
            } else {
                let mut ends = vec![0usize; ordered.len()];
                let mut start = 0usize;
                while start < ordered.len() {
                    let mut end = start;
                    while end + 1 < ordered.len()
                        && compare_keys(&ordered[end + 1].0, &ordered[start].0, &descending)
                            == std::cmp::Ordering::Equal
                    {
                        end += 1;
                    }
                    for slot in ends.iter_mut().take(end + 1).skip(start) {
                        *slot = end;
                    }
                    start = end + 1;
                }
                ends
            };

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
                // Positional functions read another row of the frame rather
                // than folding it.
                "lag" | "lead" | "first_value" | "last_value" => {
                    let args = function_arguments(f);
                    let Some(subject) = args.first() else {
                        return Err(unsupported(format!("{name}() without an argument")));
                    };
                    // A non-literal offset or default would otherwise fall
                    // back to 1 / NULL, silently answering a different
                    // question than the one asked.
                    let offset = match args.get(1) {
                        None => 1,
                        Some(argument) => expr::literal_of(argument)
                            .and_then(|v| v.as_int())
                            .ok_or_else(|| {
                                unsupported(format!("a non-constant offset in {name}()"))
                            })?,
                    };
                    // A negative offset would turn LAG into LEAD and back
                    // rather than reporting the mistake.
                    if offset < 0 {
                        return Err(EngineError::new(
                            1210,
                            "HY000",
                            format!("Incorrect arguments to {name}"),
                        ));
                    }
                    // Untagged like the subject below: `LAG(1, 1, TRUE)` over
                    // a window whose offset falls outside the partition is
                    // `[1]` on 8.3, never `[true]`.
                    let fallback = match args.get(2) {
                        None => Value::Null,
                        Some(argument) => expr::literal_of(argument)
                            .map(Value::untag_bool)
                            .ok_or_else(|| {
                                unsupported(format!("a non-constant default in {name}()"))
                            })?,
                    };

                    for (position, (_, index)) in ordered.iter().enumerate() {
                        let target = match name.as_str() {
                            "lag" => position as i64 - offset,
                            "lead" => position as i64 + offset,
                            "first_value" => 0,
                            // The default frame ends at the current row, so
                            // LAST_VALUE reads the end of the frame rather than
                            // the end of the partition — a classic trap. With
                            // tied ORDER BY keys that is the last peer, not the
                            // current row.
                            _ => frame_end[position] as i64,
                        };
                        // A window function is a window function: measured on
                        // 8.3, `FIRST_VALUE`, `LAST_VALUE`, `NTH_VALUE`, `LAG`
                        // and `LEAD` over `TRUE` are all `[1]`, exactly as
                        // `MAX` and `SUM` over one are. These read their
                        // subject straight out of a row rather than through
                        // `fold`, which is where the aggregates lose the tag,
                        // so without this the same query family answered two
                        // ways — `FIRST_VALUE(TRUE)` matched a document of
                        // `true` where `MAX(TRUE)` beside it did not.
                        out[*index] =
                            match usize::try_from(target).ok().and_then(|t| ordered.get(t)) {
                                Some((_, source)) => {
                                    // The call's own argument, which belongs to
                                    // the clause the call is written in and not
                                    // to the OVER spec beside it: `SELECT
                                    // LAG(zz.q) OVER (ORDER BY k) FROM Cl` is
                                    // 1054 in the field list on 8.3, not in the
                                    // window order by.
                                    //
                                    // A literal rather than a clause threaded in
                                    // from above, because the field list is the
                                    // only clause a window call reaches this from:
                                    // one written in an ORDER BY is settled by
                                    // `resolve_order_by` before any row is read
                                    // and reports `'order clause'` — measured,
                                    // `ORDER BY LAG(zz.q) OVER ()` is that on 8.3
                                    // too — and one in a GROUP BY or a HAVING is
                                    // 3593 or 3594 rather than 1054.
                                    let found = evaluate(subject, matching[*source], "field list")?
                                        .untag_bool();
                                    // The default is a candidate for the answer,
                                    // so it decides the sign alongside the subject
                                    // — the same all-candidates rule a conditional
                                    // follows. Measured on 8.3: `LAG(iu, 1, 0) -
                                    // 100` is -93 and `LAG(iu) - 100` raises 1690.
                                    if fallback.is_null() || matches!(fallback, Value::Unsigned(_))
                                    {
                                        found
                                    } else {
                                        found.untag_unsigned()
                                    }
                                }
                                None => fallback.clone(),
                            };
                    }
                }
                other if is_aggregate(other) => {
                    // With ORDER BY the aggregate runs to the end of the
                    // current row's peer group rather than over the whole
                    // partition, so it accumulates down the ordering. Peers
                    // share a frame, so each frame is folded once.
                    let mut position = 0usize;
                    while position < ordered.len() {
                        let end = frame_end[position];
                        let group: Vec<&Vec<Value>> = ordered[..=end]
                            .iter()
                            .map(|(_, index)| matching[*index])
                            .collect();
                        let value = self.fold(session, other, &call, refs, &group)?;
                        for (_, index) in &ordered[position..=end] {
                            out[*index] = value.clone();
                        }
                        position = end + 1;
                    }
                }
                other => return Err(unsupported(format!("window function `{other}()`"))),
            }
        }

        Ok(out)
    }

    /// Decides how one projected item is produced, and what its metadata is.
    ///
    /// `refs` is the scope a name resolves against — this query's columns and
    /// the enclosing query's — rather than `relation.refs()`, which is only
    /// this query's. A scalar subquery here is described against it, so the
    /// two are told apart: `SELECT (SELECT t.b FROM u) FROM t` reads `t.b`
    /// from the outer row and nothing else can supply it.
    fn plan_expr(
        &self,
        e: &Expr,
        alias: Option<String>,
        relation: &Relation,
        refs: &[ColumnRef],
        session: &Session,
    ) -> EngineResult<(OutColumn, Projection)> {
        // A bare column reference keeps the column's declared type, which
        // matters: clients decode a result by the type the metadata claims.
        // No unqualified fallback: `zz.id` must not quietly resolve to some
        // other table's `id`, and neither must `nodb.J2.b`. Leaving it
        // unresolved sends it down the expression path, which reports 1054.
        //
        // Through `column_reference`, so the wrappers MySQL's parser discards
        // are discarded here too: `SELECT +b`, `SELECT (b)` and `SELECT +(b)`
        // are all the column `b` on 8.3, down to the width and the flags, and
        // a `SELECT +b` sent through `CREATE TABLE ... AS` copies `b`'s
        // declaration. A variable is not a column reference even though it
        // parses as one, which is why this is not `column_parts`.
        let direct = column_reference(e)
            .and_then(column_parts)
            .and_then(|written| relation.index_of(&written));

        if let Some(i) = direct {
            let column = &relation.columns[i];
            let out = OutColumn::from_sql_type(
                alias.unwrap_or_else(|| column.name.clone()),
                &column.ty,
                column.nullable,
            )
            .in_table(&column.schema, &column.table)
            // The source column's own name, not the alias: `SELECT id AS name`
            // is `id`'s column under `name`'s name, and a `CREATE TABLE ... AS
            // SELECT` over it has to copy `id`'s attributes. The table it
            // lives in travels with it for the same reason, and is the base
            // table rather than what the query called it.
            .projected_from(&column.name, base_table(relation, &column.table));
            return Ok((out, Projection::Column(i)));
        }

        let name = alias.unwrap_or_else(|| render_expr(e));
        // A computed column is nullable because nothing here knows better —
        // except for a literal, which knows exactly. That distinction is not
        // cosmetic: a union takes the nullability of either branch, so
        // `SELECT id FROM t UNION SELECT 1` would report a NOT NULL column as
        // nullable on the strength of a `1` that can never be NULL.
        // Typed from the expression rather than reported as a string. Every
        // computed column used to be a `VAR_STRING`, which is a third of what
        // the differential harness had to say — a typed client handed back
        // `"1"` where MySQL hands back `1`.
        let sub = Some(Sub {
            engine: self,
            session,
        });
        // `refs` and not `relation.refs()`: the two differ only by the
        // enclosing query's columns, which `direct_column` cannot reach
        // anyway — their slots fall past `relation.columns` — and which a
        // scalar subquery described here does have to resolve against.
        let mut column =
            OutColumn::new(name, computed_column_type(e, refs, &relation.columns, sub));
        column.nullable = !never_null(e, refs, &relation.columns, sub);
        column.unsigned = unsigned_answer(e, refs, &relation.columns, sub);
        column.binary_values = binary_answer(e, refs, sub);
        let shape = item_shape(e, refs, &relation.columns, sub);
        column.max_length = shape.max_length;
        column.decimals = shape.decimals;
        Ok((column, Projection::Expr(Box::new(e.clone()))))
    }

    /// Evaluates a grouped query: GROUP BY, HAVING, and aggregates.
    ///
    /// An aggregate with no GROUP BY is one group covering every row, which is
    /// why both go through here.
    fn grouped(
        &self,
        session: &Session,
        select: &Select,
        input: Input<'_>,
        group_exprs: &[Expr],
        order: Option<&OrderBy>,
    ) -> EngineResult<ResultSet> {
        let Input {
            refs,
            columns: input_columns,
            matching,
        } = input;
        let sub = Some(Sub {
            engine: self,
            session,
        });
        // Kept under its own name because `resolve_group_by` returns a second
        // list — the same clause with its positions and aliases resolved — and
        // a position that names nothing has to be told apart from a select
        // item that *is* a number: after resolution both are a bare literal,
        // and `SELECT 5 FROM t GROUP BY 1` is a legal query whose group
        // expression is `5`.
        let written_groups = group_exprs;

        let items = projected_items(&select.projection)?;
        let names: Vec<String> = items.iter().map(|(alias, _)| alias.clone()).collect();

        // Before the grouping rules, because MySQL resolves names first:
        // `SELECT a.b, COUNT(*) FROM J2 GROUP BY b` is 1054 there and would
        // be 1055 if this ran after `check_full_group_by`.
        //
        // Before the GROUP BY as well, and before any row is grouped, which is
        // 8.3's order too: `SELECT nope, COUNT(*) AS n FROM Cl GROUP BY n` is
        // 1054 for `nope` there rather than the 1056 the clause below raises,
        // and `SELECT nope, k AS a, b AS a FROM Cl GROUP BY a` is 1054 rather
        // than 1052.
        if let Some(name) = items.iter().find_map(|(_, e)| unresolved_column(e, refs)) {
            return Err(EngineError::unknown_column(&name, "field list"));
        }
        refuse_at_resolve(items.iter().map(|(_, e)| e))?;
        // GROUP BY and HAVING, the two clauses #0093 left behind. Both are
        // only otherwise settled inside `for group in &groups`, so a query
        // that produces no groups never settles them: over an empty table
        // `... GROUP BY s HAVING s LIKE 'a' ESCAPE '!!'` came back as an empty
        // set where 8.3 raises 1210, and so did `GROUP BY s LIKE 'a' ESCAPE
        // '!!'`.
        //
        // HAVING is read unsubstituted because an alias is an identifier and
        // an ESCAPE lives on a `LIKE`, so aliasing cannot hide one — and where
        // it could, the expression is in the projection and the line above
        // already read it.
        //
        // Ahead of `check_full_group_by` because 8.3 answers 1210 before 1055:
        // `SELECT s FROM LE GROUP BY id HAVING s LIKE 'a' ESCAPE '!!'` is 1210
        // there, not the grouping complaint.
        //
        // Split from the HAVING's own checks rather than kept as one call,
        // because 8.3 settles the GROUP BY clause before the HAVING: `GROUP BY
        // s LIKE 'a' ESCAPE '!!' HAVING zz > 1` is 1210 there and `HAVING zz
        // LIKE 'a' ESCAPE '!!'` is 1054.
        //
        // Its names as well as its arguments now, and here rather than in the
        // grouping loop below because that loop runs once per row: over an
        // empty table `GROUP BY zz.q` came back as an empty set where 8.3
        // raises 1054, and a prepared statement was accepted that MySQL
        // refuses at prepare. #0100. `resolve_group_by` reads the list item by
        // item so the clause answers the same over an empty table as over a
        // full one, which the loop's own reading already did.
        //
        // What it hands back is the list with its positions and its select
        // aliases resolved, which is what the rest of this — the grouping
        // rules, the grouping loop, and the HAVING's scope — has to read.
        // #0229: `SELECT k AS a FROM Cl GROUP BY a` answers two rows on 8.3
        // and was 1054 here, because only the *resolver* admitted the alias
        // and the per-row evaluation still looked for a column called `a`.
        let group_exprs = resolve_group_by(written_groups, &select.projection, &items, refs)?;
        if let Some(having) = &select.having {
            check_having_scope(
                having,
                &select.projection,
                &group_exprs,
                refs,
                input_columns.len(),
            )?;
        }
        refuse_at_resolve(select.having.as_ref())?;

        // The same resolution the ungrouped path uses, and ahead of the
        // grouping rules because that is the order 8.3 answers in: `SELECT id
        // AS b FROM t GROUP BY b ORDER BY 9` is 1054 for the position there,
        // and `ORDER BY s LIKE 'a' ESCAPE '!!'` beside a GROUP BY is 1210 —
        // neither is the 1055 the grouping check would have raised.
        //
        // A resolved position binds the value already computed for it rather
        // than the expression behind it, which is the one thing this path
        // needs differently: an aggregate cannot be evaluated a second time
        // per row, and `alias_name` gives the rest of the reason.
        let out_items: Vec<OutItem> = items
            .iter()
            .map(|(name, e)| OutItem {
                name: name.clone(),
                expr: e.clone(),
                slot: column_slot(refs, e),
            })
            .collect();
        let order_items = resolve_order_by(order, &out_items, refs, true, &|index| {
            Expr::Identifier(sqlparser::ast::Ident::new(alias_name(index)))
        })?;
        let descending: Vec<bool> = order_items.iter().map(|i| i.descending).collect();

        // Under ONLY_FULL_GROUP_BY a selected column must be grouped by or
        // aggregated. Silently evaluating it against the group's first row
        // returns an arbitrary value where MySQL raises an error.
        if session.sql_mode().only_full_group_by {
            let determined = determined_columns(select, refs, &group_exprs);
            for (_, e) in &items {
                check_full_group_by(e, e, &group_exprs, refs, &determined)?;
            }
            // The ORDER BY is held to the same rule, and after the select
            // list, which is where 8.3 answers: `SELECT b, COUNT(*) FROM t
            // GROUP BY s ORDER BY b` names the *select list* there, and
            // `SELECT s AS b, COUNT(*) FROM t GROUP BY s ORDER BY b + 0` —
            // where the alias is shadowed, so the clause reads `t.b` — is
            // 1055 for the ORDER BY.
            //
            // Without this the shadowed case read `t.b` off the group's first
            // row and sorted on an arbitrary member of each group, which is
            // nondeterministic the moment a group holds more than one. It is
            // the regression the move from `substitute_aliases` to one shared
            // resolver would otherwise have introduced: that substitution
            // reached every name at any depth, so no ORDER BY on this path
            // could name an ungrouped column in the first place.
            //
            // The hidden columns a resolved position or alias binds to are in
            // scope and count as determined: they carry a value the projection
            // computed for this group, so they are grouped by construction and
            // `ORDER BY b` on the alias still answers.
            let mut scope = refs.to_vec();
            scope.extend((0..items.len()).map(|index| ColumnRef::new("", alias_name(index))));
            let mut settled = determined;
            settled.extend(refs.len()..scope.len());
            for key in &order_items {
                check_full_group_by(&key.expr, &key.expr, &group_exprs, &scope, &settled)?;
            }
        }

        // Every refusal above is settled before a single row is read, which is
        // 8.3's order and not a tidiness: it resolves the whole statement at
        // prepare, so a clause it refuses is refused over an empty table too.
        //
        // Grouping here rather than at the top of this function is what makes
        // the two agree. `GROUP BY ABS(a)` is the case that shows it: the
        // substitution reaches an identifier through an operator and not
        // through a call, so that `a` survives as a name — the resolver
        // admits it, and the 1055 for the ungrouped column behind it is what
        // 8.3 answers, over a full table and an empty one alike. With the
        // grouping first, the full table reached the key evaluation before
        // that check and answered 1054 instead.
        let mut keys: Vec<Vec<String>> = Vec::new();
        let mut groups: Vec<Vec<&Vec<Value>>> = Vec::new();

        if group_exprs.is_empty() {
            // Ungrouped aggregation always produces exactly one row, even over
            // no rows at all: `SELECT COUNT(*)` on an empty table returns 0.
            groups.push(matching.to_vec());
        } else {
            for row in matching {
                // `'group statement'` — MySQL's name for the GROUP BY clause
                // in this message, and not `'group clause'` as the pattern of
                // the others would suggest. Measured: `SELECT k FROM T GROUP
                // BY zz.q` is `Unknown column 'zz.q' in 'group statement'`.
                let ctx = Context::for_row(refs, row, session)
                    .with_executor(self)
                    .in_clause("group statement");
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

        // Column types cannot be inferred from the first group: a later group
        // may produce a different type, and the binary protocol encodes by the
        // declared type, so a client would silently decode the wrong thing.
        // Computed columns are reported as strings, as the ungrouped path does
        // — but a grouped column is still just a column, and keeps the type it
        // was declared with, or `SELECT id FROM t GROUP BY id` would describe
        // itself differently from `SELECT id FROM t`.
        let columns: Vec<OutColumn> = items
            .iter()
            .map(|(alias, e)| match direct_column(e, refs, input_columns) {
                // No table: this path has the columns and not the relation
                // that produced them, so the base table behind an alias is out
                // of reach. See `OutColumn::origin_table`.
                Some(column) => OutColumn::from_sql_type(alias, &column.ty, column.nullable)
                    .projected_from(&column.name, None),
                None => {
                    let mut column = OutColumn::new(
                        alias.clone(),
                        computed_column_type(e, refs, input_columns, sub),
                    );
                    // `COUNT` always has an answer, including zero for a group
                    // with nothing in it. Every other aggregate is nullable,
                    // even `SUM(1)`: an empty group has nothing to sum.
                    column.nullable = !never_null(e, refs, input_columns, sub);
                    column.unsigned = unsigned_answer(e, refs, input_columns, sub);
                    column.binary_values = binary_answer(e, refs, sub);
                    let shape = item_shape(e, refs, input_columns, sub);
                    column.max_length = shape.max_length;
                    column.decimals = shape.decimals;
                    column
                }
            })
            .collect();

        let mut rows: Vec<Vec<Value>> = Vec::new();
        let mut sort_keys: Vec<Vec<Value>> = Vec::new();

        for group in &groups {
            let mut out = Vec::with_capacity(items.len());
            for (_, e) in &items {
                out.push(self.eval_over_group(e, group, refs, session, &[])?);
            }

            // HAVING is evaluated after the projection so it can reference an
            // output alias, which MySQL allows and reporting queries rely on.
            if let Some(having) = &select.having {
                let resolved = substitute_aliases(having, &names);
                if self
                    .eval_over_group(&resolved, group, refs, session, &out)?
                    .truthy()
                    != Some(true)
                {
                    continue;
                }
            }

            if !order_items.is_empty() {
                let mut key = Vec::with_capacity(order_items.len());
                for item in &order_items {
                    // Already resolved against the select list, so what is
                    // left is either a hidden column carrying one of `out` or
                    // an expression over the group's rows.
                    key.push(self.eval_over_group(&item.expr, group, refs, session, &out)?);
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
        aliases: &[Value],
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
        // The projection's own values, for whatever `substitute_aliases`
        // rewrote into a reference to one. All of them, because which ones a
        // HAVING or an ORDER BY reaches for is not worth working out twice.
        for (index, value) in aliases.iter().enumerate() {
            schema.push(ColumnRef::new("", alias_name(index)));
            values.push(value.clone());
        }

        let ctx = Context::for_row(&schema, &values, session).with_executor(self);
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
                let value = self.fold(session, &name, &AggregateCall::read(f)?, refs, group)?;
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
            // The nodes sqlparser gives their own grammar. Without these an
            // aggregate inside one is found by `contains_aggregate` and then
            // never folded, so the raw `sum()` reaches the function table and
            // is reported as unsupported — `CEIL(SUM(x))` failed where
            // `CEILING(SUM(x))`, the same function spelled as a call, worked.
            Expr::Ceil { expr, field } => Expr::Ceil {
                expr: Box::new(recur(expr, folded)?),
                field: field.clone(),
            },
            Expr::Floor { expr, field } => Expr::Floor {
                expr: Box::new(recur(expr, folded)?),
                field: field.clone(),
            },
            Expr::Substring {
                expr,
                substring_from,
                substring_for,
                special,
                shorthand,
            } => Expr::Substring {
                expr: Box::new(recur(expr, folded)?),
                substring_from: match substring_from {
                    Some(from) => Some(Box::new(recur(from, folded)?)),
                    None => None,
                },
                substring_for: match substring_for {
                    Some(count) => Some(Box::new(recur(count, folded)?)),
                    None => None,
                },
                special: *special,
                shorthand: *shorthand,
            },
            Expr::Trim {
                expr,
                trim_where,
                trim_what,
                trim_characters,
            } => Expr::Trim {
                expr: Box::new(recur(expr, folded)?),
                trim_where: *trim_where,
                trim_what: match trim_what {
                    Some(what) => Some(Box::new(recur(what, folded)?)),
                    None => None,
                },
                trim_characters: trim_characters.clone(),
            },
            Expr::Position { expr, r#in } => Expr::Position {
                expr: Box::new(recur(expr, folded)?),
                r#in: Box::new(recur(r#in, folded)?),
            },
            Expr::Extract {
                field,
                syntax,
                expr,
            } => Expr::Extract {
                field: field.clone(),
                syntax: syntax.clone(),
                expr: Box::new(recur(expr, folded)?),
            },
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
        call: &AggregateCall,
        refs: &[ColumnRef],
        matching: &[&Vec<Value>],
    ) -> EngineResult<Value> {
        // Ahead of the generic pass below rather than a branch inside it,
        // because GROUP_CONCAT folds a different thing: every argument
        // concatenated per row, ordered by the call's own ORDER BY, and joined
        // with its own separator. Sharing the pass would mean evaluating the
        // first argument twice and dropping the rest.
        if name == "group_concat" {
            // The connection's collation onto the answer, for the reason
            // `functions::call` gives: a string this engine built somewhere
            // carries none of its own, and beside a literal that carries the
            // connection's the pair resolves to neither -- `HAVING
            // GROUP_CONCAT(s) = 'a,b,c'` was 1267 where 8.3 answers a row.
            // #0154.
            let mut out = self.fold_group_concat(session, call, refs, matching)?;
            out.on_connection(session.collation_connection());
            return Ok(out);
        }

        let argument = call.argument();
        let distinct = call.distinct;
        let mut values = Vec::new();
        if let Some(arg) = argument {
            for row in matching {
                let ctx = Context::for_row(refs, row, session).with_executor(self);
                // Untagged here rather than in each fold below, because no
                // aggregate keeps a boolean: measured on 8.3,
                // `JSON_ARRAY(MIN(TRUE))`, `MAX`, `SUM` and `COUNT` are all
                // `[1]` and never `[true]`. Left tagged, `SUM` also missed its
                // own integer path and answered `[2.0]` where MySQL says
                // `[2]`, because a boolean is not a `Value::Int`.
                values.push(expr::eval(arg, &ctx)?.untag_bool());
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
                } else if let Some((total, scale)) = exact_total(&present) {
                    // A total keeps the scale it added up, so `SUM` over a
                    // `DECIMAL(10,2)` is a `DECIMAL(_,2)` — `4.00`, not `4`.
                    //
                    // And it keeps the scale its argument was *shown* at,
                    // which is a different number wherever a quotient was
                    // summed: measured on 8.3 over four `DECIMAL(10,2)` rows,
                    // `SUM(a/3)` prints 5.500000 at six places and
                    // `JSON_ARRAY(SUM(a/3))` is `[5.499999999]` — the total of
                    // the nine-place values, rounded once at the end.
                    Value::decimal_at(
                        crate::decimal::normalised(crate::decimal::render(total, scale)),
                        shown_scale(&present),
                    )
                } else {
                    Value::Float(approximate_total(&present))
                }
            }
            "avg" => {
                if present.is_empty() {
                    Value::Null
                } else if let Some((total, scale)) = exact_total(&present) {
                    // Through the same division `/` uses, because MySQL's
                    // average *is* that division: the total over the row
                    // count at `div_precision_increment` extra places, so
                    // `AVG` of an integer column is a `DECIMAL(_,4)` and of a
                    // `DECIMAL(_,2)` a `DECIMAL(_,6)` at the stock 4, and both
                    // move with the variable.
                    //
                    // Not a widened total divided at the answer's own scale,
                    // which is what was here and which rounds the true
                    // quotient. MySQL truncates at a nine-digit word boundary
                    // first, and the two disagree wherever the result's scale
                    // lands on one: `AVG` over a `DECIMAL(10,5)` totalling
                    // 1.00001 across three rows is 0.333336666 on 8.3, for the
                    // 0.333336667 that rounding gives. At an increment of 0
                    // there is no word at all, so `AVG` of 1 and 2 is 1 there
                    // rather than 2.
                    let sum = crate::decimal::normalised(crate::decimal::render(total, scale));
                    // `present` is non-empty here, so the divisor cannot be
                    // the zero that is the only thing `divide` refuses a pair
                    // of decimals for; the float is what the inexact branch
                    // below would have answered.
                    match crate::decimal::divide(
                        &sum,
                        &present.len().to_string(),
                        session.div_precision_increment(),
                    ) {
                        // Shown at the same increment over the scale the
                        // *total* is shown at, which is the division's own
                        // rule: measured on 8.3, `AVG(a)` over a
                        // `DECIMAL(10,2)` prints at six places while
                        // `JSON_ARRAY(AVG(a))` carries nine, and `AVG(a/3)`
                        // prints at ten while the document carries eighteen.
                        // That second pair is #0122's example.
                        Some(mean) => Value::decimal_at(
                            mean,
                            crate::decimal::quotient_display_scale(
                                shown_scale(&present),
                                session.div_precision_increment(),
                            ),
                        ),
                        None => Value::Float(approximate_total(&present) / present.len() as f64),
                    }
                } else {
                    Value::Float(approximate_total(&present) / present.len() as f64)
                }
            }
            // By `compare` and not by the sort order, which is measured: over
            // `ENUM('b','a')` holding both members, `ORDER BY e` puts `b`
            // first and `MIN(e)` is `a`.
            //
            // The winner comes back untagged, which is measured too:
            // `MIN(e) + 0` is 0 on 8.3 where the bare `e + 0` is the ordinal.
            "min" | "max" => {
                let want_max = name == "max";
                present
                    .into_iter()
                    .cloned()
                    .map(Value::untag_enum)
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
            // Unlike GROUP_CONCAT this keeps each value's type, so numbers
            // stay numbers in the document — and it keeps NULLs, which become
            // JSON nulls rather than being dropped.
            "json_arrayagg" => {
                if distinct {
                    return Err(unsupported("DISTINCT in JSON_ARRAYAGG()"));
                }
                if matching.is_empty() {
                    Value::Null
                } else {
                    let items = values.iter().map(crate::json::from_value).collect();
                    crate::json::render(&serde_json::Value::Array(items))
                }
            }
            // Nothing here needs the connection's collation put on it the way
            // `GROUP_CONCAT` above does, and that was checked rather than
            // assumed: `COUNT`, `SUM` and `AVG` answer numbers, `JSON_ARRAYAGG`
            // answers a `Value::Json`, and `MIN` and `MAX` hand back one of
            // their inputs, which arrived carrying whatever collation it had.
            // `GROUP_CONCAT` is the only one that builds a string of its own.
            other => return Err(unsupported(format!("aggregate `{other}()`"))),
        })
    }

    /// Folds `GROUP_CONCAT` over the matching rows.
    ///
    /// Every part of this was measured on 8.3, because every part of it was
    /// being thrown away — #0088.
    ///
    /// - A row's contribution is *all* the arguments concatenated, under
    ///   `CONCAT`'s NULL rule: `GROUP_CONCAT(k, s)` over a row whose `s` is
    ///   NULL contributes nothing at all, not `k` alone.
    /// - A row contributing NULL is dropped, and a group that contributes
    ///   nothing is NULL rather than the empty string.
    /// - DISTINCT de-duplicates on that concatenated value and keeps the
    ///   *first* row of each: over `('a',5),('b',2),('a',1),('c',9)`,
    ///   `GROUP_CONCAT(DISTINCT s ORDER BY n)` is `b,a,c` on 8.3, which is the
    ///   5 and not the 1.
    /// - With DISTINCT and no ORDER BY the answer comes back sorted by the
    ///   value: `a,B` and not `B,a` for `'B','a'`, and `9,10,100` and not
    ///   `10,100,9` for the numbers, so it is the same comparison ORDER BY
    ///   uses and not a text one.
    /// - The join is the call's separator, and the whole answer is truncated
    ///   to `group_concat_max_len` bytes.
    ///
    /// One thing deliberately *not* matched: 8.3 breaks a tie between two rows
    /// with equal sort keys by putting the later row first — over four rows
    /// tied on the key, `a,b,c,d` comes back `d,c,b,a`, and a constant key
    /// like `ORDER BY 1+1` reverses the whole group. That is its insert-order
    /// tree showing through rather than an order it defines, so this sorts
    /// stably and leaves tied rows as they arrived. Nothing in the suite or
    /// the corpus ties, for the reason the differential README gives about
    /// orders that are not total.
    fn fold_group_concat(
        &self,
        session: &Session,
        call: &AggregateCall,
        refs: &[ColumnRef],
        matching: &[&Vec<Value>],
    ) -> EngineResult<Value> {
        // The concatenated value and this row's sort keys, in arrival order.
        let mut rows: Vec<(Value, Vec<Value>)> = Vec::with_capacity(matching.len());
        for row in matching {
            let ctx = Context::for_row(refs, row, session).with_executor(self);
            let mut parts = Vec::with_capacity(call.arguments.len());
            for argument in &call.arguments {
                parts.push(expr::eval(argument, &ctx)?.untag_bool());
            }
            // A single argument keeps its own value rather than becoming text,
            // because the DISTINCT ordering below compares it: `GROUP_CONCAT
            // (DISTINCT n)` over 9, 10 and 100 is `9,10,100` on 8.3 and would
            // be `10,100,9` compared as strings.
            let value = match parts.len() {
                1 => parts.remove(0),
                _ if parts.iter().any(Value::is_null) => Value::Null,
                _ => Value::text(parts.iter().map(Value::as_text).collect::<String>()),
            };
            if value.is_null() {
                continue;
            }
            let mut keys = Vec::with_capacity(call.order.len());
            for key in &call.order {
                keys.push(expr::eval(&key.expr, &ctx)?);
            }
            rows.push((value, keys));
        }

        if call.distinct {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            rows.retain(|(value, _)| seen.insert(value.collation_key()));
        }

        if !call.order.is_empty() {
            let descending: Vec<bool> = call.order.iter().map(|k| k.descending).collect();
            rows.sort_by(|a, b| compare_keys(&a.1, &b.1, &descending));
        } else if call.distinct {
            // Sorted only because DISTINCT sorts on 8.3; without it the rows
            // come back in arrival order, which is why this is not simply
            // always done. `sort_cmp` is what `compare_keys` reaches for once
            // it has ruled the NULLs out, and every NULL is already gone here.
            rows.sort_by(|a, b| a.0.sort_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        }

        if rows.is_empty() {
            return Ok(Value::Null);
        }

        let joined = rows
            .iter()
            .map(|(value, _)| value.as_text())
            .collect::<Vec<_>>()
            .join(&call.separator);
        Ok(Value::text(truncate_bytes(
            joined,
            session.group_concat_max_len(),
        )))
    }

    /// Sorts an already-projected result, for ORDER BY over output aliases.
    ///
    /// `carried` is what the source rows already answered for each key, one
    /// entry per row of `result` in its own order, with `None` where the
    /// source row could not answer that key. Empty for a caller that never had
    /// source rows to ask — a set operation, whose branches are already
    /// projected by the time their common ORDER BY is read.
    ///
    /// A carried value is used in preference to anything derivable here,
    /// because a key that resolved against a source row may name a column the
    /// select list does not return: `SELECT id, ROW_NUMBER() OVER () rn FROM t
    /// ORDER BY 2, b` sorts by `b`, and the output has no `b` in it. Deriving
    /// every key from the output refused that statement with 1054 for `b`
    /// where 8.3 answers it — and each of its two keys works alone, so only
    /// the mixture failed.
    fn order_projected(
        &self,
        result: &mut ResultSet,
        session: &Session,
        order: Option<&OrderBy>,
        carried: &[Vec<Option<Value>>],
    ) -> EngineResult<()> {
        let Some(order) = order else { return Ok(()) };
        let OrderByKind::Expressions(items) = &order.kind else {
            return Err(unsupported("this ORDER BY form"));
        };

        // Sorting happens over the projected rows, so ORDER BY may only
        // reference output columns. That covers what Prisma emits.
        //
        // Without a database, deliberately: a projected result is in none, and
        // 8.3 treats it the way it treats a derived table — `SELECT b FROM
        // (SELECT b FROM J2) d ORDER BY nodb.d.b` answers there. Where the
        // clause does address a FROM item, `resolve_order_by` has already
        // resolved it against the relation, database included.
        let refs: Vec<ColumnRef> = result
            .columns
            .iter()
            .map(|c| ColumnRef::new(&c.table, &c.name))
            .collect();

        let mut keyed: Vec<(Vec<Value>, Vec<Value>)> = Vec::with_capacity(result.rows.len());
        for (index, row) in result.rows.iter().enumerate() {
            let ctx = Context::for_row(&refs, row, session).with_executor(self);
            let mut key = Vec::with_capacity(items.len());
            for (position, item) in items.iter().enumerate() {
                if let Some(value) = carried
                    .get(index)
                    .and_then(|row| row.get(position))
                    .and_then(Option::as_ref)
                {
                    key.push(value.clone());
                    continue;
                }
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
        // Through `split_name`, like every other statement, rather than by
        // splitting the rendering of `insert.table` on `.`. That could not
        // tell a qualifier from a dot inside a name, so an INSERT into a table
        // spelled ``my.table`` was 1049 `Unknown database 'my'` — against a
        // table `CREATE TABLE` and `SELECT` had both accepted, so the engine
        // disagreed with itself about one name.
        let sqlparser::ast::TableObject::TableName(table_name_ast) = &insert.table else {
            return Err(unsupported(format!("INSERT INTO {}", insert.table)));
        };
        let (db, table_name) = self.split_name(session, table_name_ast)?;
        let ignore = insert.ignore;

        let duplicate_key_update = match &insert.on {
            None => None,
            Some(sqlparser::ast::OnInsert::DuplicateKeyUpdate(assignments)) => Some(assignments),
            Some(other) => return Err(unsupported(format!("INSERT ... {other}"))),
        };

        // Whether the source is a written VALUES list, which is what settles
        // the scope `ON DUPLICATE KEY UPDATE` resolves its values in — see the
        // guard below, where the difference is measured.
        let mut written_values = false;

        // `INSERT ... SELECT` runs its query first and to completion. MySQL
        // reads the whole source before writing, so a statement whose SELECT
        // reads the table it inserts into sees the table as it was.
        let source_rows: Vec<Vec<SourceValue>> = match &insert.source {
            Some(query) => match query.body.as_ref() {
                SetExpr::Values(values) => {
                    written_values = true;
                    // The written values are the one clause with no row to
                    // evaluate against — they *are* the row — so an item 8.3
                    // settles while resolving has to be settled here or not at
                    // all. Both of these were measured: `INSERT INTO K VALUES
                    // (SIGN(1, 2))` is 1582 on 8.3 and wrote a 1 here, and
                    // `INSERT INTO K VALUES (('a' LIKE 'b' ESCAPE '!!'))` is
                    // 1210 there and wrote a 0.
                    //
                    // Ahead of the table lookup, which is 8.3's order for the
                    // parameter count and *not* for the escape: `INSERT INTO
                    // nosuch VALUES (SIGN(1, 2))` is 1582 there and `INSERT
                    // INTO nosuch VALUES ('a' LIKE 'b' ESCAPE '!!')` is 1146.
                    // 1582 comes out of 8.3's parser, which is the gap
                    // `wrong_parameter_count` already records against 1146;
                    // the escape does not, so this line is 1210 where 8.3
                    // says 1146. Measured and left — moving it would mean
                    // splitting the written values the way the clause below
                    // is split, and the written values are #0102's half.
                    refuse_at_resolve(values.rows.iter().flat_map(|row| row.iter()))?;
                    // Only the parameter count of the clause's values here, for
                    // exactly that reason: `INSERT INTO nosuch (k) VALUES (1)
                    // ON DUPLICATE KEY UPDATE k = SIGN(1, 2)` is 1582 on 8.3
                    // and the same statement with `('a' LIKE 'b' ESCAPE '!!')`
                    // is 1146. Everything else the clause is held to is
                    // settled below, once the table's columns are known.
                    if let Some(bad) = duplicate_key_update
                        .into_iter()
                        .flatten()
                        .find_map(|a| wrong_parameter_count(&a.value))
                    {
                        return Err(bad);
                    }
                    values
                        .rows
                        .iter()
                        .map(|row| {
                            row.iter()
                                .cloned()
                                .map(|e| SourceValue::Expr(Box::new(e)))
                                .collect()
                        })
                        .collect()
                }
                _ => {
                    let selected = self.query(session, query)?;
                    selected
                        .rows
                        .into_iter()
                        .map(|row| row.into_iter().map(SourceValue::Value).collect())
                        .collect()
                }
            },
            None => return Err(unsupported("INSERT without VALUES")),
        };

        let session_snapshot = session.clone();
        let _writing = self.begin_write()?;
        let mut working = self.database_copy(&db)?;

        // Captured before the child table is borrowed mutably, so validation
        // is a pure function of the candidate row.
        let mut fk_parents = {
            let table = working
                .table(&table_name)
                .ok_or_else(|| EngineError::no_such_table(&table_name))?;
            foreign_keys::snapshot(&working, table)
        };

        let table = working
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
                    // The parts, not the join of them: MySQL takes `INSERT
                    // INTO q (q.a, q.b) VALUES (1, 2)` — measured on 8.3, and
                    // `INSERT INTO db.q (db.q.a)` with it — where matching on
                    // the whole name is 1054 for a column called `q.a` that no
                    // table has.
                    //
                    // The qualifier is *compared* rather than dropped, though,
                    // which is what this used to do: `INSERT INTO J2
                    // (nodb.J2.a)` and `INSERT INTO J2 (K.a)` are both 1054 on
                    // 8.3 and both wrote to `J2`.`a` here.
                    let parts = name::parts(c);
                    let values: Vec<&str> = parts.iter().map(String::as_str).collect();
                    let written = name::split(&values).ok_or_else(|| {
                        EngineError::unknown_column(&parts.join("."), "field list")
                    })?;
                    let name = written.name.to_owned();
                    let index = table
                        .columns
                        .iter()
                        .position(|column| {
                            ColumnRef::in_database(&db, &table_name, &column.name).matches(&written)
                        })
                        .ok_or_else(|| {
                            EngineError::unknown_column(&written.written(), "field list")
                        })?;
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
        // The *first* id the statement generated, which is what MySQL reports
        // — measured on 8.3, where a three-row insert into an empty
        // AUTO_INCREMENT table answers `LAST_INSERT_ID()` with 1 and not 3, and
        // the same for a two-row `INSERT ... SELECT`. This had been the last
        // one, which put the engine at odds with its own CTAS path: #0125 gave
        // `fill_new_table` the first-id rule and deliberately left INSERT
        // alone, so the two disagreed with each other. #0132.
        //
        // Zero is the "nothing generated" sentinel rather than an `Option`,
        // because it is also the value the OK packet carries in that case;
        // `assign_auto_increment` counts from one and can never produce it.
        let mut first_id = 0u64;

        // The two things 8.3 settles about the *statement* before it writes
        // anything, rather than about a row as it reaches one. Measured: an
        // insert whose second value list is the wrong length, or whose second
        // value list writes to a generated column, stores nothing and leaves
        // `LAST_INSERT_ID()` exactly as it found it — where a row refused
        // during execution (a value the column cannot take, a NOT NULL, a
        // duplicate key) does report the id the rows before it were given.
        // Left in the row loop, they consumed a counter value and reported an
        // id on the way to failing.
        for (n, values) in source_rows.iter().enumerate() {
            // MySQL rejects a row whose value count does not match the column
            // list. Truncating instead stores corrupt data for a client bug
            // that would have failed loudly against the real server.
            if values.len() != targets.len() {
                return Err(EngineError::new(
                    1136,
                    "21S01",
                    format!("Column count doesn't match value count at row {}", n + 1),
                ));
            }
            for (slot, source) in targets.iter().zip(values.iter()) {
                let column = &table.columns[*slot];
                if column.generated.is_some() && !writes_nothing(source) {
                    return Err(EngineError::new(
                        3105,
                        "HY000",
                        format!(
                            "The value specified for generated column '{}' in table '{}' \
                             is not allowed.",
                            column.name, table.name
                        ),
                    ));
                }
            }
        }

        // `ON DUPLICATE KEY UPDATE`, resolved before any row is read — because
        // 8.3 settles the whole clause there, and because nothing here reads
        // it otherwise until a row actually collides. That is #0009's
        // recurring shape wearing its DML coat: the assignments live behind a
        // condition the data decides, so on a table nothing collides with —
        // an empty one, a row with a different key, a table with no unique key
        // at all — none of them was read and `INSERT INTO Bare (id, k) VALUES
        // (1, 1) ON DUPLICATE KEY UPDATE k = a.b` inserted and reported
        // success where 8.3 raises 1054. #0125 hoisted the *targets* out for
        // that reason; #0165 is the values beside them.
        //
        // Below the two statement-level checks above rather than above them,
        // which is 8.3's order and was measured: `INSERT INTO Dup (id, k)
        // VALUES (1, 1), (2) ON DUPLICATE KEY UPDATE k = zz` is 1136 there and
        // `INSERT INTO Gen VALUES (1, 1, 1) ON DUPLICATE KEY UPDATE k = zz` is
        // 3105, not 1054 for `zz` in either. The insert's own column list is
        // resolved earlier still, so `INSERT INTO Gen (g) ...` is 3105 and
        // `INSERT INTO Dup (nope) ...` is 1054 for `nope`.
        let duplicate_targets: Vec<usize> = match duplicate_key_update {
            None => Vec::new(),
            Some(assignments) => assignments
                .iter()
                .map(|assignment| {
                    // The qualifier compared rather than dropped, as in the
                    // column list above: `... ON DUPLICATE KEY UPDATE
                    // nodb.J2.b = 1` is 1054 on 8.3 and wrote to `J2`.`b`
                    // here.
                    let parts = assignment_parts(&assignment.target)?;
                    let values: Vec<&str> = parts.iter().map(String::as_str).collect();
                    let written = name::split(&values).ok_or_else(|| {
                        EngineError::unknown_column(&parts.join("."), "field list")
                    })?;
                    table
                        .columns
                        .iter()
                        .position(|column| {
                            ColumnRef::in_database(&db, &table.name, &column.name).matches(&written)
                        })
                        .ok_or_else(|| {
                            EngineError::unknown_column(&written.written(), "field list")
                        })
                })
                .collect::<EngineResult<_>>()?,
        };

        // The values, after *every* target: 8.3 settles the whole target list
        // first, measured on `... ON DUPLICATE KEY UPDATE k = zz2, zz1 = 1`,
        // which names `zz1` there. That is the opposite of the order the same
        // function runs in from `update`, and both are measured — see
        // `resolve_assigned_values`.
        //
        // The scope is the *target table's* columns and nothing else, which is
        // the half #0102 left this clause out for and is why it is measured
        // here rather than copied from the WHERE path. All of these are 8.3:
        //
        // | in the clause | 8.3 |
        // |---|---|
        // | `k = s`, a column the insert did not name | resolves |
        // | `k = VALUES(s)`, likewise | resolves |
        // | `k = VALUES(nosuch)` | 1054 `nosuch` |
        // | `k = T.k`, `k = thisdb.T.k` | resolve |
        // | `k = nodb.T.k` | 1054 `nodb.T.k` |
        //
        // So `VALUES(x)` names a column of the table rather than a position in
        // the insert's column list, and the insert's column list adds nothing
        // to the scope — it can only name columns the table already has.
        // `unresolved_column` reads a call's arguments in the scope around it,
        // which is exactly that, so `VALUES(...)` needs no case of its own.
        //
        // Two sources are left alone rather than resolved, each because its
        // scope is *wider* than this and refusing on this one would refuse
        // legal SQL — the failure #0102's review caught and the direction that
        // matters:
        //
        // - `INSERT ... SELECT`, where the clause also sees the SELECT's FROM
        //   tables: `INSERT INTO E (id, k) SELECT a, b FROM Src x ON DUPLICATE
        //   KEY UPDATE k = a` writes `Src`.`a` on 8.3 and `k = x.b` reads the
        //   alias, while `k = aa` for an output alias and `k = Src.b` under
        //   `Src x` are both 1054 there — one flat scope with the target
        //   table, so `k = k + 1` over a source that also has a `k` is 1052.
        //   A set operation contributes nothing: the same clause over `SELECT
        //   ... UNION SELECT ...` is 1054 for `a`.
        // - the row alias form, `VALUES (1, 1) AS new ON DUPLICATE KEY UPDATE
        //   k = new.k`, which 8.3 accepts. Nothing here models `new`, so
        //   resolving against the table alone would refuse it.
        //
        // Both are gaps of their own — this engine reads neither, so the two
        // still answer where 8.3 refuses an unknown name in them — and both
        // are a wrong *answer* left as it was rather than a wrong refusal
        // added.
        if written_values && insert.insert_alias.is_none() {
            if let Some(assignments) = duplicate_key_update {
                let refs: Vec<ColumnRef> = table
                    .columns
                    .iter()
                    .map(|c| ColumnRef::in_database(&db, &table.name, &c.name))
                    .collect();
                resolve_assigned_values(assignments, &refs)?;
            }
        }

        // Immediately invoked so that a row refused halfway through still
        // reaches the two lines below it: 8.3 keeps the id a statement
        // generated even when a later row makes it store nothing, and a `?`
        // out of the middle of a loop cannot run anything on its way past.
        // The borrows it takes end at the semicolon, which is what gives
        // `session` and `working` back.
        let stored = (|| -> EngineResult<()> {
            // IGNORE covers a value the column cannot hold as well as the
            // constraint violations handled below: 8.3 stores `abc` for an
            // `INSERT IGNORE` of `'abcdef'` into a `VARCHAR(3)`, and 127 for
            // 300 into a TINYINT, each with a warning rather than an error.
            let _ignoring = ignore.then(crate::types::WriteScope::ignoring);
            for (nth, values) in source_rows.iter().enumerate() {
                // Named on the error a value that does not fit raises: 8.3
                // counts the rows of the statement, so a three-row INSERT
                // whose second row is too wide says `at row 2`.
                let _row = crate::types::WriteScope::row(nth + 1);
                let ctx = Context::rowless(&session_snapshot).with_executor(self);
                let mut row = default_row(table, &ctx)?;

                for (slot, source) in targets.iter().zip(values.iter()) {
                    if table.columns[*slot].generated.is_some() {
                        // Anything that writes a value here was refused above, and
                        // what is left keeps the expression's own answer.
                        continue;
                    }
                    // DEFAULT in a VALUES list keeps the column's default.
                    if matches!(source, SourceValue::Expr(e)
                    if matches!(e.as_ref(), Expr::Identifier(i)
                        if i.value.eq_ignore_ascii_case("default")))
                    {
                        continue;
                    }
                    let value = match source {
                        SourceValue::Expr(e) => expr::eval(e, &ctx)?,
                        SourceValue::Value(v) => v.clone(),
                    };
                    let column = &table.columns[*slot];
                    row[*slot] = column.ty.store(value, &column.name)?;
                }

                let generated_id = assign_auto_increment(table, &mut row)?;

                // IGNORE downgrades a strict-mode error to a warning, so a NOT
                // NULL column left out of the statement takes its type's implicit
                // default rather than failing. The real migrations rely on this:
                // they omit `NOT NULL` columns that have no DEFAULT.
                if ignore {
                    for (slot, column) in table.columns.iter().enumerate() {
                        if !column.nullable && row[slot].is_null() && column.generated.is_none() {
                            row[slot] = column.ty.implicit_default();
                        }
                    }
                }

                // Generated columns, NOT NULL and CHECK are all validated here, on
                // the same path UPDATE uses.
                let row = self.finalise_row(table, row, &session_snapshot)?;

                // `ON DUPLICATE KEY UPDATE` turns a collision into an update of
                // the row that was already there, rather than an error. Checked
                // before the constraints below, because a collision is the
                // expected case for it rather than a failure.
                if let Some(assignments) = duplicate_key_update {
                    if let Some(existing) = table.conflicting_row(&row) {
                        let before = table.rows[existing].clone();
                        let mut updated = before.clone();
                        let updated_refs: Vec<ColumnRef> = table
                            .columns
                            .iter()
                            .map(|c| ColumnRef::in_database(&db, &table.name, &c.name))
                            .collect();

                        let mut assigned: Vec<usize> = Vec::with_capacity(assignments.len());
                        for (assignment, slot) in assignments.iter().zip(&duplicate_targets) {
                            let slot = *slot;
                            let column_name = table.columns[slot].name.clone();
                            // The right-hand side sees the row as it stands, which
                            // is what `c = c + 1` means here — while `VALUES(c)`
                            // names the value the insert would have written, which
                            // is the ordinary way to spell an upsert.
                            let ctx = Context::for_row(&updated_refs, &updated, &session_snapshot)
                                .with_executor(self);
                            let value = match values_reference(&assignment.value) {
                                Some(referenced) => {
                                    let from =
                                        table.column_index(&referenced).ok_or_else(|| {
                                            EngineError::unknown_column(&referenced, "field list")
                                        })?;
                                    row[from].clone()
                                }
                                None => expr::eval(&assignment.value, &ctx)?,
                            };
                            updated[slot] = table.columns[slot].ty.store(value, &column_name)?;
                            assigned.push(slot);
                        }

                        // An upsert that updates is an update, and MySQL moves
                        // an `ON UPDATE CURRENT_TIMESTAMP` column for it:
                        // measured on 8.3, `INSERT ... ON DUPLICATE KEY UPDATE
                        // v = v + 1` over a row whose timestamp defaulted to
                        // 2020-01-01 leaves it there here and restamps it
                        // there. #0193, and it is the same call the two UPDATE
                        // paths make.
                        //
                        // Before the comparison below and not after, which is
                        // the half of this that is not additive. #0089
                        // established that MySQL decides "changed" *after* the
                        // restamp, so the two have an order; restamping
                        // afterwards would fix the timestamp and leave the
                        // count reading a row the statement had already
                        // finished with. `restamp_on_update` is itself a no-op
                        // when nothing else moved, so an upsert that changes
                        // nothing still answers 0 — measured, and the reason
                        // the order works out rather than a coincidence.
                        Self::restamp_on_update(
                            table,
                            &before,
                            &mut updated,
                            &assigned,
                            &session_snapshot,
                        )?;

                        let updated = self.finalise_row(table, updated, &session_snapshot)?;
                        foreign_keys::check(&fk_parents, &updated, &table_name)?;
                        // Against the table's own rows, skipping the one being
                        // updated — cloning them here would copy the whole table
                        // once per conflicting row of a multi-row insert.
                        table.check_unique(&updated, Some(existing))?;

                        // MySQL reports two affected rows for a row it updated this
                        // way, and one for a row it inserted. A row it left as it
                        // was counts as nothing — unless the connection negotiated
                        // CLIENT_FOUND_ROWS, which makes the same row count one.
                        // Measured on 8.3: a repeated `INSERT INTO D VALUES (1,5)
                        // ON DUPLICATE KEY UPDATE v = VALUES(v)` answers 0 without
                        // the flag and 1 with it.
                        if updated != before {
                            table.rows[existing] = updated;
                            inserted += 2;
                        } else if session_snapshot.found_rows {
                            inserted += 1;
                        }
                        continue;
                    }
                }

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
                // After the row is stored, so a row the statement did not keep
                // does not report an id: an `INSERT IGNORE` whose first row
                // collides answers with the id of the first row it actually
                // inserted, and an `ON DUPLICATE KEY UPDATE` that only updated
                // reports nothing at all. Both measured on 8.3.
                if let Some(id) = generated_id {
                    if first_id == 0 {
                        first_id = id;
                    }
                }
                inserted += 1;
            }
            Ok(())
        })();

        // Reported even though nothing was stored, and measured that way:
        // `INSERT INTO F (k) VALUES (2), (1), (3)` against a unique `k`
        // already holding 1 answers 1062, leaves the table as it was, and
        // still answers `LAST_INSERT_ID()` with the id row one was given. A
        // ROLLBACK does not take it back either.
        if let Err(e) = stored {
            if first_id != 0 {
                session.last_insert_id = first_id;
            }
            return Err(e);
        }

        self.commit_database(&db, working)?;
        mark_modified(session, &db, std::slice::from_ref(&table_name));
        session.affected_rows = inserted;
        // After the commit, so an engine that could not store the rows does
        // not report an id for them. The session is left alone when the
        // statement generated no id at all — an upsert that updated rather
        // than inserted, or an insert that supplied every id itself, has none
        // to report but has not erased the one before it either. The OK packet
        // still carries 0, which is what MySQL sends.
        if first_id != 0 {
            session.last_insert_id = first_id;
        }
        Ok(Outcome::Affected {
            rows: inserted,
            last_insert_id: first_id,
        })
    }

    fn update(
        &self,
        session: &mut Session,
        table: &TableWithJoins,
        assignments: &[Assignment],
        selection: Option<&Expr>,
        order_by: &[OrderByExpr],
        limit: Option<&Expr>,
    ) -> EngineResult<Outcome> {
        if !table.joins.is_empty() {
            // MySQL refuses both on the multi-table form, because "the first
            // row" is not defined across a join.
            if !order_by.is_empty() || limit.is_some() {
                return Err(unsupported(
                    "ORDER BY or LIMIT on a multi-table UPDATE, which MySQL does not allow either",
                ));
            }
            return self.update_joined(session, table, assignments, selection);
        }
        let TableFactor::Table { name, alias, .. } = &table.relation else {
            return Err(unsupported("this UPDATE target"));
        };
        let (db, table_name) = self.split_name(session, name)?;
        // `UPDATE Board b ... WHERE b.x` qualifies by the alias, so that is
        // the name the row's columns have to answer to.
        let addressed_as = alias
            .as_ref()
            .map(|a| a.name.value.clone())
            .unwrap_or_else(|| table_name.clone());

        let session_snapshot = session.clone();

        // Applied to a copy and swapped in only on success, so a RESTRICT
        // found while cascading to children cannot leave the parent updated.
        let _writing = self.begin_write()?;
        let mut working = self.database_copy(&db)?;

        let fk_parents = {
            let table = working
                .table(&table_name)
                .ok_or_else(|| EngineError::no_such_table(&table_name))?;
            foreign_keys::snapshot(&working, table)
        };

        let table = working
            .table_mut(&table_name)
            .ok_or_else(|| EngineError::no_such_table(&table_name))?;

        // Carrying the database, so a reference qualified by the wrong one is
        // refused rather than read as this table's: `UPDATE J2 SET b = b WHERE
        // nodb.J2.a = 1` is 1054 on 8.3. Over an *empty* table nothing
        // evaluates these at all, which is why the guard below reads this
        // rather than leaving it to the row loop.
        let refs: Vec<ColumnRef> = table
            .columns
            .iter()
            .map(|c| ColumnRef::in_database(&db, &addressed_as, &c.name))
            .collect();

        // Before any row is considered, and after the table has been resolved
        // because 8.3 answers 1146 for a missing table ahead of both of these.
        // The predicate and the assigned values are otherwise only read per
        // row, so over an empty table `UPDATE LE SET id = 2 WHERE s LIKE 'a'
        // ESCAPE '!!'` reported nothing affected where 8.3 raises 1210, and
        // `UPDATE Bare SET k = 1 WHERE a.b = 1` nothing where it raises 1054 —
        // and a caller cannot tell either from a WHERE nothing matched, which
        // is what makes the silence worse on a write than on a query.
        //
        // Ahead of the assignment targets, which is 8.3's order for the
        // predicate in both codes: `UPDATE LE SET nope = 'x' WHERE s LIKE 'a'
        // ESCAPE '!!'` is 1210 there and `UPDATE Bare SET nope1 = 1 WHERE
        // nope2 = 1` names `nope2` in the where clause, not `nope1`.
        //
        // The WHERE before the values, because 8.3 settles it first.
        resolve_dml_where(selection, &refs)?;
        resolve_assigned_values(assignments, &refs)?;

        // Resolved once, before any row is considered: whether writing to a
        // generated column is an error must not depend on the data, and
        // neither must whether the target names a column at all.
        //
        // Through the same matcher every clause uses, so the qualifier is
        // compared rather than dropped. Reading only the last part —
        // `assignment_column` — made `UPDATE J2 SET nodb.J2.b = 1` and
        // `UPDATE J2 SET nosuch.b = 1` write to `J2`.`b`, where 8.3 answers
        // 1054 for both. `refs` carries the database this statement's table is
        // in, which is what makes the first of those reachable from here.
        let targets: Vec<usize> = assignments
            .iter()
            .map(|assignment| {
                let parts = assignment_parts(&assignment.target)?;
                let values: Vec<&str> = parts.iter().map(String::as_str).collect();
                let written = name::split(&values)
                    .ok_or_else(|| EngineError::unknown_column(&parts.join("."), "field list"))?;
                let slot = refs
                    .iter()
                    .position(|c| c.matches(&written))
                    .ok_or_else(|| EngineError::unknown_column(&written.written(), "field list"))?;
                if table.columns[slot].generated.is_some() {
                    return Err(EngineError::new(
                        3105,
                        "HY000",
                        format!(
                            "The value specified for generated column '{}' in table \
                             '{}' is not allowed.",
                            written.name, table.name
                        ),
                    ));
                }
                Ok(slot)
            })
            .collect::<EngineResult<_>>()?;

        // Both counts, because which of them the client is told depends on the
        // capability it negotiated rather than on the statement. See
        // `Session::found_rows`.
        let mut matched_rows = 0u64;
        let mut changed_rows = 0u64;
        let mut changed: Vec<(Vec<Value>, Vec<Value>)> = Vec::new();
        let mut rows = table.rows.clone();

        // Which rows to write is settled before any of them is written. The
        // predicate only ever reads its own row, so this sees exactly what the
        // single pass did — but ORDER BY and LIMIT need the whole matching set
        // in hand before the first write.
        let mut matching = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let ctx = Context::for_row(&refs, row, &session_snapshot).with_executor(self);
            let matched = match selection {
                Some(p) => expr::eval(p, &ctx)?.truthy() == Some(true),
                None => true,
            };
            if matched {
                matching.push(index);
            }
        }
        let chosen =
            self.dml_row_order(&session_snapshot, &refs, &rows, matching, order_by, limit)?;

        for (nth, index) in chosen.into_iter().enumerate() {
            // The rows in the order this statement writes them, which is what
            // 8.3 counts: `UPDATE ... ORDER BY k DESC` failing on the row it
            // reaches first says `at row 1` whatever that row's key is.
            let _row = crate::types::WriteScope::row(nth + 1);
            let ctx = Context::for_row(&refs, &rows[index], &session_snapshot).with_executor(self);

            let mut candidate = rows[index].clone();
            let mut assigned: Vec<usize> = Vec::with_capacity(assignments.len());
            for (assignment, slot) in assignments.iter().zip(&targets) {
                let slot = *slot;
                // The right-hand side sees the row as it was before this
                // statement, so `SET a = b, b = a` swaps rather than copies.
                let value = expr::eval(&assignment.value, &ctx)?;
                let column = &table.columns[slot];
                candidate[slot] = column.ty.store(value, &column.name)?;
                assigned.push(slot);
            }
            Self::restamp_on_update(
                table,
                &rows[index],
                &mut candidate,
                &assigned,
                &session_snapshot,
            )?;

            let candidate = self.finalise_row(table, candidate, &session_snapshot)?;
            foreign_keys::check(&fk_parents, &candidate, &table_name)?;
            table.check_unique_against(&rows, &candidate, index)?;
            // Compared after `finalise_row` and the restamp above, so what is
            // weighed is the row as it will be stored: a coercion that lands
            // back on the value already there — `SET n = '7'` over an INT
            // holding 7 — is not a change, and MySQL does not count one.
            //
            // Plain value equality, deliberately not the collation-aware
            // comparison: MySQL counts a case-only rewrite as a change even
            // where the two values compare equal under the column's
            // collation. Measured on 8.3 — a `varchar` holding 'A', `UPDATE U
            // SET y = 'a' WHERE y = 'A'` reports one row changed.
            matched_rows += 1;
            if candidate != rows[index] {
                changed_rows += 1;
            }
            // Children follow the key, so both halves of the change are needed.
            changed.push((rows[index].clone(), candidate.clone()));
            rows[index] = candidate;
        }

        table.rows = rows;
        let mut touched = vec![table_name.clone()];
        cascade_update(&mut working, &table_name, &changed, &mut touched)?;
        self.commit_database(&db, working)?;
        mark_modified(session, &db, &touched);

        let affected = session.rows_affected_by_update(matched_rows, changed_rows);
        session.affected_rows = affected;
        Ok(Outcome::affected(affected))
    }

    /// `UPDATE a JOIN b ON ... SET a.x = ..., b.y = ...`.
    ///
    /// The join is materialised, so each output row remembers which base row it
    /// came from; without that there is nothing to write back to.
    fn update_joined(
        &self,
        session: &mut Session,
        from: &TableWithJoins,
        assignments: &[Assignment],
        selection: Option<&Expr>,
    ) -> EngineResult<Outcome> {
        let db = session
            .current_database
            .clone()
            .ok_or_else(EngineError::no_database)?;
        let session_snapshot = session.clone();

        let relation = self.build_relation(&session_snapshot, from)?;
        reject_duplicate_sources(&relation)?;
        let refs = relation.refs();

        // As on the single-table path, and after the relation is built and its
        // sources checked so a missing table is still 1146 and a repeated
        // alias still 1066 — 8.3 answers both ahead of the WHERE's own 1054,
        // measured on `UPDATE S a JOIN T a ON 1 SET a.k = 1 WHERE nope = 1`.
        // The ON clauses of the join are settled inside `join()`; this is the
        // WHERE and the assigned values.
        resolve_dml_where(selection, &refs)?;
        resolve_assigned_values(assignments, &refs)?;

        // Each assignment is resolved to a source table and a column within it.
        struct Target {
            source: usize,
            column: String,
            value: Expr,
        }
        let mut targets: Vec<Target> = Vec::with_capacity(assignments.len());
        for assignment in assignments {
            // Every part, so a target written `nodb.x.b` is refused rather
            // than writing to `x`.`b`: an assignment whose database is wrong
            // is the worst shape of this bug, because it writes.
            let target = assignment_parts(&assignment.target)?;
            let values: Vec<&str> = target.iter().map(String::as_str).collect();
            let written = name::split(&values)
                .ok_or_else(|| EngineError::unknown_column(&target.join("."), "field list"))?;
            let name = written.name;

            // An unqualified name that matches more than one joined table is
            // ambiguous; picking the first would write to an arbitrary one.
            if written.table.is_none() {
                let matches = relation
                    .columns
                    .iter()
                    .filter(|c| c.name.eq_ignore_ascii_case(name))
                    .count();
                if matches > 1 {
                    return Err(EngineError::new(
                        1052,
                        "23000",
                        format!("Column '{name}' in field list is ambiguous"),
                    ));
                }
            }

            let index = relation
                .index_of(&written)
                // Every part, as `ColumnParts::written` exists for: naming the
                // column alone hides which qualifier was the problem, and 8.3
                // reports `nodb.a.b` here.
                .ok_or_else(|| EngineError::unknown_column(&written.written(), "field list"))?;
            let source = relation
                .source_index(&relation.columns[index].table)
                .ok_or_else(|| unsupported("updating a derived table or view"))?;
            // The same wrong write `delete_joined` refuses, on the sibling
            // path: the row is written back into `db`'s copy under the table's
            // *name*, so `UPDATE other.T JOIN K ON other.T.a = K.a SET
            // other.T.z = 42` changed `db`.`T` and reported one row affected —
            // 8.3 changes `other`.`T` and leaves this database alone. One
            // database is copied and committed per statement, so this refuses
            // rather than writing to whichever one shares the name.
            let holds = relation
                .source_holds(source)
                .ok_or_else(|| unsupported("a write to this FROM item"))?;
            if !holds.eq_ignore_ascii_case(&db) {
                return Err(unsupported(format!(
                    "a multi-table UPDATE of `{holds}` while `{db}` is the current database"
                )));
            }
            targets.push(Target {
                source,
                column: relation.columns[index].name.clone(),
                value: assignment.value.clone(),
            });
        }

        let _writing = self.begin_write()?;
        let mut working = self.database_copy(&db)?;

        // Parent keys are captured per table before anything is written, as in
        // the single-table path.
        let mut parents: BTreeMap<usize, Vec<foreign_keys::ParentKeys>> = BTreeMap::new();
        {
            let database = &working;
            for target in &targets {
                let base = &relation.base_tables[target.source];
                let table = database
                    .table(base)
                    .ok_or_else(|| EngineError::no_such_table(base))?;

                // Every target is checked, not just the first per table, and
                // before any row is examined: whether writing to a generated
                // column is an error must not depend on whether a row matched.
                if table
                    .column(&target.column)
                    .is_some_and(|c| c.generated.is_some())
                {
                    return Err(EngineError::new(
                        3105,
                        "HY000",
                        format!(
                            "The value specified for generated column '{}' in table '{base}' \
                             is not allowed.",
                            target.column
                        ),
                    ));
                }

                parents
                    .entry(target.source)
                    .or_insert_with(|| foreign_keys::snapshot(database, table));
            }
        }

        // MySQL updates each base row at most once however many join rows it
        // appears in, and every assignment to that table applies to it.
        let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();
        // Both counts, for the reason the single-table path keeps both: which
        // one the client is told is the capability's decision, not this
        // statement's. Measured on 8.3 over two joined rows apiece, `UPDATE A
        // JOIN B ON A.id = B.id SET A.v = A.v, B.v = B.v` reports 4 with
        // CLIENT_FOUND_ROWS and 0 without.
        let mut matched_rows = 0u64;
        let mut changed_rows = 0u64;
        let mut changes: BTreeMap<usize, Vec<MovedRow>> = BTreeMap::new();

        for (row_index, row) in relation.rows.iter().enumerate() {
            let ctx = Context::for_row(&refs, row, &session_snapshot).with_executor(self);
            let matched = match selection {
                Some(p) => expr::eval(p, &ctx)?.truthy() == Some(true),
                None => true,
            };
            if !matched {
                continue;
            }

            for source in 0..relation.sources.len() {
                if !targets.iter().any(|t| t.source == source) {
                    continue;
                }
                let Some(Some(origin)) = relation.origins[row_index].get(source).copied() else {
                    continue;
                };
                let base = relation.base_tables[source].clone();
                // Keyed by the base table, not the join position: a self-join
                // reaches one physical row through two aliases, and MySQL
                // updates — and counts — it once.
                if !seen.insert((base.to_ascii_lowercase(), origin)) {
                    continue;
                }

                let table = working
                    .table_mut(&base)
                    .ok_or_else(|| EngineError::no_such_table(&base))?;

                let before = table.rows[origin].clone();
                let mut candidate = before.clone();
                let mut assigned: Vec<usize> = Vec::new();
                for target in targets.iter().filter(|t| t.source == source) {
                    let slot = table
                        .column_index(&target.column)
                        .ok_or_else(|| EngineError::unknown_column(&target.column, "field list"))?;
                    let value = expr::eval(&target.value, &ctx)?;
                    candidate[slot] = table.columns[slot].ty.store(value, &target.column)?;
                    assigned.push(slot);
                }
                Self::restamp_on_update(
                    table,
                    &before,
                    &mut candidate,
                    &assigned,
                    &session_snapshot,
                )?;

                let candidate = self.finalise_row(table, candidate, &session_snapshot)?;
                if let Some(snapshot) = parents.get(&source) {
                    foreign_keys::check(snapshot, &candidate, &base)?;
                }
                let rows = table.rows.clone();
                table.check_unique_against(&rows, &candidate, origin)?;

                // Counted per row, not per assignment.
                matched_rows += 1;
                if candidate != before {
                    changed_rows += 1;
                }

                table.rows[origin] = candidate.clone();
                changes.entry(source).or_default().push((before, candidate));
            }
        }

        // Children follow a changed parent key here too.
        let mut touched: Vec<String> = relation.base_tables.clone();
        for (source, moved) in &changes {
            let base = relation.base_tables[*source].clone();
            cascade_update(&mut working, &base, moved, &mut touched)?;
        }

        self.commit_database(&db, working)?;
        mark_modified(session, &db, &touched);

        let affected = session.rows_affected_by_update(matched_rows, changed_rows);
        session.affected_rows = affected;
        Ok(Outcome::affected(affected))
    }

    fn delete(
        &self,
        session: &mut Session,
        delete: &sqlparser::ast::Delete,
    ) -> EngineResult<Outcome> {
        if delete.returning.is_some() {
            return Err(unsupported("RETURNING"));
        }

        let from = match &delete.from {
            sqlparser::ast::FromTable::WithFromKeyword(t)
            | sqlparser::ast::FromTable::WithoutKeyword(t) => t,
        };

        // MySQL writes a multi-table DELETE two ways, and they disagree about
        // where the targets live: `DELETE a FROM a JOIN b` names them before
        // FROM, and `DELETE FROM a USING a JOIN b` names them after it. Both
        // reach the same executor with the targets pulled out first.
        if let Some(using) = &delete.using {
            // The two spellings name their targets in different places, so a
            // statement using both says two different things at once. MySQL
            // rejects it, and taking either half silently would delete from a
            // table the other half did not name.
            if !delete.tables.is_empty() {
                return Err(EngineError::parse(
                    "a DELETE names its targets either before FROM or after it, not both: \
                     write `DELETE a FROM a JOIN b ON ...` or `DELETE FROM a USING a JOIN b ON ...`",
                ));
            }
            let targets = from
                .iter()
                .map(target_written)
                .collect::<EngineResult<Vec<DeleteTarget>>>()?;
            return self.delete_joined(session, delete, using, &targets);
        }
        // A comma list is a cross join here as it is anywhere else, so
        // `DELETE t1 FROM t1, t2 WHERE t1.id = t2.id` — the commoner spelling
        // of a multi-table delete — takes the same path as the JOIN form.
        if from.len() > 1 || !from[0].joins.is_empty() || !delete.tables.is_empty() {
            let targets = delete
                .tables
                .iter()
                .map(DeleteTarget::written)
                .collect::<EngineResult<Vec<DeleteTarget>>>()?;
            return self.delete_joined(session, delete, from, &targets);
        }
        let TableFactor::Table { name, alias, .. } = &from[0].relation else {
            return Err(unsupported("this DELETE target"));
        };
        let (db, table_name) = self.split_name(session, name)?;
        // As in UPDATE, an alias is what a qualified reference names.
        let addressed_as = alias
            .as_ref()
            .map(|a| a.name.value.clone())
            .unwrap_or_else(|| table_name.clone());

        let session_snapshot = session.clone();

        // The delete and its cascade are applied to a copy and swapped in only
        // if the whole thing succeeds. A RESTRICT discovered part-way through a
        // chain would otherwise leave the parent already removed and earlier
        // children already cascaded.
        let _writing = self.begin_write()?;
        let mut working = self.database_copy(&db)?;
        let table = working
            .table_mut(&table_name)
            .ok_or_else(|| EngineError::no_such_table(&table_name))?;

        // With the database, as on the UPDATE path.
        let refs: Vec<ColumnRef> = table
            .columns
            .iter()
            .map(|c| ColumnRef::in_database(&db, &addressed_as, &c.name))
            .collect();

        // As on the UPDATE path, and for the same reason: the predicate is
        // only read per row, so `DELETE FROM LE WHERE s LIKE 'a' ESCAPE '!!'`
        // over an empty table reported nothing deleted where 8.3 raises 1210,
        // and `DELETE FROM Bare WHERE a.b = 1` nothing where it raises 1054.
        resolve_dml_where(delete.selection.as_ref(), &refs)?;

        let mut matching = Vec::new();
        for (index, row) in table.rows.iter().enumerate() {
            let ctx = Context::for_row(&refs, row, &session_snapshot).with_executor(self);
            let matched = match &delete.selection {
                Some(p) => expr::eval(p, &ctx)?.truthy() == Some(true),
                None => true,
            };
            if matched {
                matching.push(index);
            }
        }
        let chosen = self.dml_row_order(
            &session_snapshot,
            &refs,
            &table.rows,
            matching,
            &delete.order_by,
            delete.limit.as_ref(),
        )?;

        // Removed in the order chosen, so a cascade follows the same order the
        // statement asked for; kept in storage order, which is the table's.
        //
        // One pass over a position map rather than a set plus a clone of every
        // doomed row: `DELETE FROM t` with no WHERE takes all of them, and a
        // suite clearing a table between cases should not pay for a second
        // copy of it.
        const KEEP: usize = usize::MAX;
        let mut slot_of = vec![KEEP; table.rows.len()];
        for (slot, index) in chosen.iter().enumerate() {
            slot_of[*index] = slot;
        }
        let mut removed_rows: Vec<Vec<Value>> = vec![Vec::new(); chosen.len()];
        let mut kept: Vec<Vec<Value>> = Vec::with_capacity(table.rows.len() - chosen.len());
        for (index, row) in std::mem::take(&mut table.rows).into_iter().enumerate() {
            match slot_of[index] {
                KEEP => kept.push(row),
                slot => removed_rows[slot] = row,
            }
        }

        let removed = removed_rows.len() as u64;
        table.rows = kept;

        // Children are handled after the parent rows are removed, so a
        // self-referencing table does not see the rows it is cascading from.
        let mut touched = vec![table_name.clone()];
        cascade_delete(&mut working, &table_name, &removed_rows, &mut touched)?;
        self.commit_database(&db, working)?;
        mark_modified(session, &db, &touched);

        session.affected_rows = removed;
        Ok(Outcome::affected(removed))
    }

    /// `DELETE a FROM a JOIN b ON ...`, where the named tables are the ones
    /// rows are removed from and the join only decides which.
    ///
    /// `targets` is that list of names as they were written, already pulled
    /// from whichever half of the statement the spelling put it in.
    fn delete_joined(
        &self,
        session: &mut Session,
        delete: &sqlparser::ast::Delete,
        from: &[TableWithJoins],
        targets: &[DeleteTarget],
    ) -> EngineResult<Outcome> {
        // Neither is defined across a join, and MySQL refuses both here too.
        if !delete.order_by.is_empty() || delete.limit.is_some() {
            return Err(unsupported(
                "ORDER BY or LIMIT on a multi-table DELETE, which MySQL does not allow either",
            ));
        }
        let db = session
            .current_database
            .clone()
            .ok_or_else(EngineError::no_database)?;
        let session_snapshot = session.clone();

        let relation = self.build_relation_list(&session_snapshot, from)?;
        let refs = relation.refs();

        // MySQL rejects `DELETE FROM a JOIN b` outright: with a join, the
        // target has to be named. Defaulting to every table in the join would
        // empty tables nobody asked to touch.
        if targets.is_empty() {
            return Err(EngineError::parse(
                "a multi-table DELETE must name the tables to delete from, \
                 as in `DELETE a FROM a JOIN b ON ...`",
            ));
        }
        reject_duplicate_sources(&relation)?;

        // `DELETE t, t FROM ...` names one table twice; MySQL refuses it rather
        // than deleting from it twice.
        //
        // Over the whole list before any of it is resolved, because that is
        // 8.3's order: `DELETE nodb.A, nodb.A FROM A JOIN B ON A.id = B.id` is
        // 1066 there, not the 1109 that resolving the first target would give,
        // and `DELETE A, Nope, A` is 1066 on the third target rather than 1109
        // on the second. Two spellings of one table count as the same target —
        // in a session on `db`, `DELETE db.A, A` is 1066 — so the comparison
        // is against the database each one *means* rather than the one it
        // wrote, which is `same_as`.
        for (i, target) in targets.iter().enumerate() {
            if targets[..i]
                .iter()
                .any(|earlier| earlier.same_as(target, &db))
            {
                return Err(EngineError::new(
                    1066,
                    "42000",
                    format!("Not unique table/alias: '{}'", target.name),
                ));
            }
        }

        let mut sources: Vec<usize> = Vec::with_capacity(targets.len());
        for target in targets {
            // 1109 rather than 1146, measured: the catalog holds a table by
            // this name and the failure is that the FROM clause does not put
            // *that* one in scope. The qualifier is the reason it may not —
            // `DELETE nodb.A FROM A JOIN B ON A.id = B.id` used to delete from
            // the current schema, a write aimed at another one landing here.
            let source = relation
                .delete_target(target.database.as_deref(), &db, &target.name)
                .ok_or_else(|| EngineError::unknown_multi_delete_target(&target.name))?;
            // The rows come out of `db`'s copy below, keyed by table name
            // alone, so a target whose FROM item is in another database would
            // have rows removed from *this* one — the same wrong write the
            // qualifier check exists to stop, in the direction the name cannot
            // see because both halves are spelled correctly. 8.3 deletes from
            // the other database and this engine commits one at a time, so it
            // refuses rather than guessing which.
            //
            // Reading across databases is untouched: only a target is checked,
            // and `DELETE J2 FROM J2 JOIN other.T ON J2.a = other.T.a` still
            // deletes from `J2`.
            let holds = relation
                .source_holds(source)
                .ok_or_else(|| unsupported("a write to this FROM item"))?;
            if !holds.eq_ignore_ascii_case(&db) {
                return Err(unsupported(format!(
                    "a multi-table DELETE from `{holds}` while `{db}` is the current database"
                )));
            }
            sources.push(source);
        }

        // The multi-table half of the same clause. The join's own ON is
        // settled in `join()`.
        //
        // Below the targets rather than beside the relation, which is where
        // #0087 first put the ESCAPE half of it, because 8.3 settles which
        // tables a multi-table DELETE names before it looks at the predicate:
        // `DELETE a, a FROM LE a JOIN LF b ON a.id = b.id WHERE a.s LIKE 'x'
        // ESCAPE '!!'` is 1066 there and was 1210 here, and adding the name
        // check at the old point would have made the same statement with an
        // unknown column 1054 where 8.3 still says 1066.
        resolve_dml_where(delete.selection.as_ref(), &refs)?;

        // Collect first, delete after: removing as we go would shift the
        // indices the remaining join rows point at.
        let mut doomed: Vec<std::collections::HashSet<usize>> =
            vec![std::collections::HashSet::new(); relation.sources.len()];

        for (row_index, row) in relation.rows.iter().enumerate() {
            let ctx = Context::for_row(&refs, row, &session_snapshot).with_executor(self);
            let matched = match &delete.selection {
                Some(p) => expr::eval(p, &ctx)?.truthy() == Some(true),
                None => true,
            };
            if !matched {
                continue;
            }
            for source in &sources {
                if let Some(Some(origin)) = relation.origins[row_index].get(*source).copied() {
                    doomed[*source].insert(origin);
                }
            }
        }

        let _writing = self.begin_write()?;
        let mut working = self.database_copy(&db)?;

        let mut removed = 0u64;
        let mut touched: Vec<String> = Vec::new();
        let mut cascades: Vec<(String, Vec<Vec<Value>>)> = Vec::new();

        // Two aliases of one table are two sources over a single row vector, so
        // the sets are merged by base table. Deleting per source would apply
        // the second set's pre-deletion indices to an already-shortened table.
        let mut by_table: BTreeMap<String, std::collections::HashSet<usize>> = BTreeMap::new();
        for source in &sources {
            let indices = std::mem::take(&mut doomed[*source]);
            by_table
                .entry(relation.base_tables[*source].clone())
                .or_default()
                .extend(indices);
        }

        // Every named table's own rows go first. Cascading in between would
        // shift the row indices the remaining sets were captured against.
        for (table_name, indices) in by_table {
            if indices.is_empty() {
                continue;
            }

            let table = working
                .table_mut(&table_name)
                .ok_or_else(|| EngineError::no_such_table(&table_name))?;
            let mut removed_rows = Vec::new();
            let mut kept = Vec::with_capacity(table.rows.len());
            for (index, row) in std::mem::take(&mut table.rows).into_iter().enumerate() {
                if indices.contains(&index) {
                    removed_rows.push(row);
                } else {
                    kept.push(row);
                }
            }
            table.rows = kept;
            removed += removed_rows.len() as u64;

            touched.push(table_name.clone());
            cascades.push((table_name, removed_rows));
        }

        for (table_name, removed_rows) in cascades {
            cascade_delete(&mut working, &table_name, &removed_rows, &mut touched)?;
        }

        self.commit_database(&db, working)?;
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
        // `DROP INDEX x ON t` carries the table separately from the index name.
        on_table: Option<&ObjectName>,
    ) -> EngineResult<Outcome> {
        match object_type {
            ObjectType::Database | ObjectType::Schema => {
                for name in names {
                    self.catalog_mut()?
                        .drop_database(&name::dotted(name), if_exists)?;
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
            ObjectType::Index => {
                // Without the table there is nothing to look the index up in.
                // `DROP EVENT IF EXISTS` also lands here, against an event that
                // no longer exists, and accepting it as a no-op is enough.
                let Some(on_table) = on_table else {
                    if if_exists {
                        return Ok(Outcome::ok());
                    }
                    return Err(unsupported("DROP INDEX without ON <table>"));
                };
                let (db, table_name) = self.split_name(session, on_table)?;
                let mut catalog = self.catalog_mut()?;
                let database = catalog
                    .database_mut(&db)
                    .ok_or_else(|| EngineError::unknown_database(&db))?;
                let stored = database
                    .table(&table_name)
                    .ok_or_else(|| EngineError::no_such_table(&table_name))?;

                // Applied to a copy and swapped in only once the foreign key
                // rule below has passed, for the reason `ddl::alter_table`
                // works on one: this statement either happens or does not, and
                // a refusal must not leave the earlier names dropped.
                let mut table = stored.clone();
                let original_indexes = table.indexes.clone();
                let dropped: Vec<String> = names.iter().map(name::dotted).collect();
                ddl::drop_indexes_named(&mut table, &dropped, if_exists)?;
                // The same rule the ALTER TABLE spelling reads, and 8.3 reads
                // it here too: `DROP INDEX fk ON C` is 1553 there. This arm
                // walked away with it until the rule had one home. No renames
                // to carry, since this statement only drops.
                ddl::refuse_unbacked_foreign_key(
                    database,
                    &table_name,
                    &table,
                    &original_indexes,
                    &[],
                )?;

                let key = database
                    .tables
                    .keys()
                    .find(|k| k.eq_ignore_ascii_case(&table_name))
                    .cloned();
                if let Some(key) = key {
                    database.tables.insert(key, table);
                }
                mark_modified(session, &db, std::slice::from_ref(&table_name));
            }
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
                let name = name::dotted(variable);
                let ctx = Context::rowless(session).with_executor(self);
                // An expression that cannot be evaluated must not become a
                // silent NULL: the variable would then carry a wrong answer
                // into whatever reads it next.
                let value = match values.first() {
                    Some(e) => match unquoted_collation(&name, e) {
                        Some(named) => named,
                        None => expr::eval(e, &ctx)?,
                    },
                    None => Value::Null,
                };
                self.assign(session, &name, value)?;
                Ok(Outcome::ok())
            }
            S::MultipleAssignments { assignments } => {
                for assignment in assignments {
                    let name = name::dotted(&assignment.name);
                    let ctx = Context::rowless(session).with_executor(self);
                    let value = match unquoted_collation(&name, &assignment.value) {
                        Some(named) => named,
                        None => expr::eval(&assignment.value, &ctx)?,
                    };
                    self.assign(session, &name, value)?;
                }
                Ok(Outcome::ok())
            }
            // `SET NAMES` settles the connection's collation, which is what
            // every literal in the session compares and folds under — so it
            // changes answers rather than only what `@@collation_connection`
            // reports. Measured on 8.3 across one `SET NAMES`: `'a ' = 'a'`
            // goes from 0 to 1 and `HEX(LOWER('ẞ'))` from `C39F` to `E1BA9E`.
            // It was swallowed here with the rest, which is half of #0154.
            S::SetNames {
                charset_name,
                collation_name,
            } => {
                session.set_names(&charset_name.value, collation_name.as_deref())?;
                Ok(Outcome::ok())
            }
            // `SET NAMES DEFAULT` restores the server's, measured.
            S::SetNamesDefault {} => {
                session.set_names("utf8mb4", None)?;
                Ok(Outcome::ok())
            }
            // SET TRANSACTION and friends need no state here.
            _ => Ok(Outcome::ok()),
        }
    }

    /// A `SET` of one variable.
    ///
    /// Fallible because a system variable can refuse its value: MySQL answers
    /// 1232 for a `default_week_format` that is not an integer, and this
    /// engine took it silently until the week had somewhere to be wrong.
    fn assign(&self, session: &mut Session, name: &str, value: Value) -> EngineResult<()> {
        match name.strip_prefix("@@") {
            Some(rest) => session.set_system_variable(rest, &value)?,
            None => match name.strip_prefix('@') {
                Some(rest) => session.set_user_variable(rest, value),
                // A bare name in SET is a system variable.
                None => session.set_system_variable(name, &value)?,
            },
        }
        Ok(())
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
                .push(vec![Value::text(name), Value::text(value)]);
        }
        result
    }

    fn show_databases(&self) -> EngineResult<ResultSet> {
        let mut result = ResultSet::new(vec![OutColumn::new("Database", ColumnType::VarString)]);
        result.rows.push(vec![Value::text(INFORMATION_SCHEMA)]);
        for name in self.catalog()?.database_names() {
            result.rows.push(vec![Value::text(name)]);
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
            result.rows.push(vec![Value::text(name.clone())]);
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
                    Value::text(column.name.clone()),
                    Value::text(column.ty.declared.clone()),
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
                Value::text(column.name.clone()),
                Value::text(column.ty.declared.clone()),
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

    /// Runs a query and presents its rows as a named relation.
    ///
    /// A CTE is addressed like a table, so its result needs the same shape a
    /// table has: named columns carrying their real types, and no base rows,
    /// because there is nothing to write back to.
    fn relation_of_query(
        &self,
        session: &Session,
        query: &Query,
        name: &str,
    ) -> EngineResult<Relation> {
        let result = self.query(session, query)?;
        Ok(self.relation_from_result(session, &result.columns, result.rows, query, name))
    }

    /// The relation a result set stands as, under the name a qualified
    /// reference will use.
    ///
    /// Split from [`Self::relation_of_query`] because a recursive CTE needs
    /// the shape before it has the rows: the anchor alone types the columns,
    /// and every later round is written through those types. Both callers go
    /// through here so the two cannot describe a CTE differently.
    ///
    /// The columns and the rows arrive apart because the recursive caller
    /// keeps its result set — it still has a LIMIT to apply to the rows this
    /// produces — and the ordinary one does not.
    fn relation_from_result(
        &self,
        session: &Session,
        columns: &[OutColumn],
        rows: Vec<Vec<Value>>,
        query: &Query,
        name: &str,
    ) -> Relation {
        // As for a derived table, which a CTE resolves like in every other
        // respect too. See `materialised_by_mysql`.
        let narrowed = materialised_by_mysql(query);
        let schema = session.current_database.clone().unwrap_or_default();
        let columns = columns
            .iter()
            .map(|c| crate::relation::RelColumn {
                schema: schema.clone(),
                // A CTE resolves like a derived table and not like a table:
                // measured on 8.3, `WITH c AS (...) SELECT nodb.c.b FROM c`
                // answers and `SELECT probe.c.*` from it is 1051, with the
                // current database spelled correctly. The wire metadata above
                // still carries the database, which is why these are two
                // fields.
                qualifier_database: String::new(),
                table: name.to_owned(),
                name: c.name.clone(),
                ty: if narrowed {
                    c.materialised_sql_type()
                } else {
                    c.sql_type()
                },
                nullable: c.nullable,
            })
            .collect();

        Relation {
            origins: vec![Vec::new(); rows.len()],
            columns,
            rows: materialised(rows),
            sources: Vec::new(),
            base_tables: Vec::new(),
            source_databases: Vec::new(),
        }
    }

    /// Expands a JSON document into rows, one per match of the row path.
    ///
    /// Each column takes its value from a path evaluated against that match,
    /// and `FOR ORDINALITY` numbers the rows from one. A column whose path
    /// matches nothing is NULL, which is MySQL's default and the only sensible
    /// reading of "this element has no such member".
    fn json_table(
        &self,
        session: &Session,
        document: &Expr,
        row_path: &sqlparser::ast::ValueWithSpan,
        columns: &[sqlparser::ast::JsonTableColumn],
        alias: Option<&sqlparser::ast::TableAlias>,
    ) -> EngineResult<Relation> {
        use sqlparser::ast::JsonTableColumn as Column;

        let name = alias
            .map(|a| a.name.value.clone())
            .unwrap_or_else(|| "json_table".to_owned());

        // The document usually comes from a column of a table named earlier
        // in the FROM, which reaches here as the lateral row.
        let outer = session.lateral.clone();
        let ctx = match &outer {
            Some(row) => Context::for_row(&row.schema, &row.values, session).with_executor(self),
            None => Context::rowless(session).with_executor(self),
        };
        let source = expr::eval(document, &ctx)?;
        let parsed = crate::json::parse(&source)?;
        let path = crate::json::parse_path(&literal_text(row_path))?;

        // Described before any row is read, so an empty document still yields
        // the right shape rather than a relation with no columns.
        let mut rel_columns = Vec::with_capacity(columns.len());
        for column in columns {
            let (column_name, ty) = match column {
                Column::Named(named) => (
                    named.name.value.clone(),
                    crate::types::SqlType::from_ast(&named.r#type)?,
                ),
                Column::ForOrdinality(ident) => (
                    ident.value.clone(),
                    crate::types::SqlType::from_ast(&sqlparser::ast::DataType::BigInt(None))?,
                ),
                Column::Nested(_) => return Err(unsupported("NESTED PATH in JSON_TABLE")),
            };
            rel_columns.push(crate::relation::RelColumn {
                schema: session.current_database.clone().unwrap_or_default(),
                // A JSON_TABLE is a derived table, and resolves like one:
                // `SELECT nodb.jt.v FROM JSON_TABLE(...) jt` answers on 8.3
                // and `SELECT probe.jt.*` is 1051.
                qualifier_database: String::new(),
                table: name.clone(),
                name: column_name,
                ty,
                nullable: true,
            });
        }

        let mut rows = Vec::new();
        for (ordinal, matched) in crate::json::select(&parsed, &path).into_iter().enumerate() {
            let mut row = Vec::with_capacity(columns.len());
            for column in columns {
                row.push(match column {
                    Column::ForOrdinality(_) => Value::Int(ordinal as i64 + 1),
                    Column::Named(named) => {
                        if named.exists {
                            return Err(unsupported("EXISTS in a JSON_TABLE column"));
                        }
                        // `DEFAULT x ON EMPTY` and `ERROR ON ERROR` change what
                        // a column holds when its path matches nothing, so
                        // accepting and ignoring them would answer NULL where
                        // MySQL answers a default or refuses outright.
                        if named.on_empty.is_some() || named.on_error.is_some() {
                            return Err(unsupported(
                                "ON EMPTY and ON ERROR in a JSON_TABLE column",
                            ));
                        }
                        let column_path = crate::json::parse_path(&literal_text(&named.path))?;
                        // A wildcard can select several values, and a column
                        // holds one. MySQL rejects it rather than picking.
                        if column_path.is_multiple() {
                            return Err(EngineError::new(
                                3153,
                                "42000",
                                "In this situation, path expressions may not contain the * \
                                 or ** tokens or an array range",
                            ));
                        }
                        match crate::json::select(matched, &column_path).first() {
                            // Unquoted, because a column declared VARCHAR wants
                            // the string rather than the string in quotes.
                            Some(found) => crate::json::unquote(found),
                            None => Value::Null,
                        }
                    }
                    Column::Nested(_) => return Err(unsupported("NESTED PATH in JSON_TABLE")),
                });
            }
            rows.push(row);
        }

        // Coerced after the fact so a column declared INT is an integer, which
        // is what a join against a real column has to compare against.
        for row in &mut rows {
            for (value, column) in row.iter_mut().zip(&rel_columns) {
                if !value.is_null() {
                    *value = column.ty.coerce(value.clone(), &column.name)?;
                }
            }
        }

        Ok(Relation {
            origins: vec![Vec::new(); rows.len()],
            columns: rel_columns,
            rows,
            sources: Vec::new(),
            base_tables: Vec::new(),
            source_databases: Vec::new(),
        })
    }

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

        // Not through `execute`, which reconciles on the way out: this result
        // is not on its way out, it is about to feed another query. Reconciling
        // it shapes every value for the *inner* column's code, and the outer
        // query then reads it under a code of its own — a `double(10,4)` in a
        // view came back rendered at its four places, was converted to a plain
        // double by the DOUBLE arm on the second pass, and lost them, while
        // the `float(10,4)` beside it kept them. Same rule, two answers,
        // decided by which arm the second pass happened to have.
        let outcome = self.execute_at_depth(&mut scoped, &view.definition, &[], 0)?;
        let Outcome::Rows(rows) = outcome else {
            return Err(EngineError::new(
                1064,
                "42000",
                format!("View `{}` does not return rows", view.name),
            ));
        };

        // Whether reading the view hands back a temporary table's columns or
        // the expressions MySQL would have merged into the reading query. The
        // stored shape cannot answer it: that one is always the materialised
        // shape. See `materialised_by_mysql`.
        //
        // Asked only where an answer could change something. Answering it
        // costs a rewrite and a parse of the definition, on top of the one
        // `execute_at_depth` has just done, and no column narrows without a
        // width -- which almost none have.
        let narrowed = rows.columns.iter().any(OutColumn::narrows)
            && definition_materialised_by_mysql(&scoped, &view.definition);

        // Explicit column names from `CREATE VIEW v (a, b)` rename the output.
        let columns = rows
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| crate::relation::RelColumn {
                schema: database.to_owned(),
                // A view is in a database, unlike the query behind it: 8.3
                // answers `SELECT probe.V.b FROM V` and refuses
                // `SELECT nodb.V.b`, and `SELECT probe.V.*` is a result rather
                // than 1051.
                qualifier_database: database.to_owned(),
                table: alias.to_owned(),
                name: view
                    .columns
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| c.name.clone()),
                // The inner query already worked out each column's type;
                // discarding it reports every view column as text, and a typed
                // client then decodes a number as a string.
                ty: if narrowed {
                    c.materialised_sql_type()
                } else {
                    c.sql_type()
                },
                nullable: c.nullable,
            })
            .collect();

        Ok(Relation {
            // A view is materialised, so it has no base row to write back to.
            origins: vec![Vec::new(); rows.rows.len()],
            columns,
            rows: rows.rows,
            sources: Vec::new(),
            base_tables: Vec::new(),
            source_databases: Vec::new(),
        })
    }

    /// Restamps `ON UPDATE CURRENT_TIMESTAMP` columns that the statement did
    /// not set itself.
    ///
    /// MySQL only restamps when the row actually changes, so an UPDATE that
    /// writes the same values leaves the timestamp alone.
    fn restamp_on_update(
        table: &Table,
        before: &[Value],
        candidate: &mut [Value],
        assigned: &[usize],
        session: &Session,
    ) -> EngineResult<()> {
        if before == candidate {
            return Ok(());
        }
        for (slot, column) in table.columns.iter().enumerate() {
            if column.on_update_now && !assigned.contains(&slot) {
                let now = crate::functions::call("now", Vec::new(), session)?;
                candidate[slot] = column.ty.store(now, &column.name)?;
            }
        }
        Ok(())
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
                let ctx = Context::for_row(&refs, &row, session).with_executor(self);
                expr::eval(&parsed, &ctx)?
            };
            let column = &table.columns[index];
            row[index] = column.ty.store(value, &column.name)?;
        }

        for (index, column) in table.columns.iter().enumerate() {
            if !column.nullable && row[index].is_null() {
                return Err(EngineError::null_not_allowed(&column.name));
            }
        }

        for check in &table.checks {
            let parsed = parse_expression(&check.expr)?;
            let ctx = Context::for_row(&refs, &row, session).with_executor(self);
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

    /// Splits `db.table` into parts, falling back to the session's database.
    fn split_name(&self, session: &Session, name: &ObjectName) -> EngineResult<(String, String)> {
        self.qualify(session, name::parts(name))
    }

    /// Resolves an already-read name against the session's current database.
    ///
    /// Takes the parts rather than a `db.table` string, because a part may
    /// hold a dot of its own and nothing can split one back apart. The joined
    /// spelling is built only in the arm that reports it: every table factor
    /// in every query comes through here, and rendering a message for the
    /// statements that are fine cost a second walk and a third allocation on
    /// the hottest path in the engine.
    fn qualify(&self, session: &Session, mut parts: Vec<String>) -> EngineResult<(String, String)> {
        match parts.len() {
            1 => {
                let db = session
                    .current_database
                    .clone()
                    .ok_or_else(EngineError::no_database)?;
                Ok((db, parts.remove(0)))
            }
            2 => {
                let table = parts.remove(1);
                Ok((parts.remove(0), table))
            }
            _ => Err(unsupported(format!("qualified name `{}`", parts.join(".")))),
        }
    }
}

/// The collation of the stored column behind a projected one.
///
/// The *resolved* one rather than the clause the column was written with,
/// because a view reports what its base column compares under and that can
/// come from the table: measured on 8.3, a view over `CREATE TABLE tb (a
/// VARCHAR(10)) COLLATE=utf8mb4_bin` reports `utf8mb4_bin` for its column,
/// where reading the column's own clause found none and reported the
/// database's.
///
/// `None` unless the projection names a real column of a real table, which is
/// what `origin` and `origin_table` record.
fn base_collation(catalog: &Catalog, column: &OutColumn) -> Option<String> {
    let table = column.origin_table.as_deref()?;
    let name = column.origin.as_deref()?;
    let source = catalog
        .database(&column.schema)?
        .table(table)?
        .columns
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))?;
    // Only for a column that has one at all: `collation_name` answers NULL for
    // an INT, and handing it a name here would make a view claim one.
    matches!(
        source.ty.kind,
        crate::types::TypeKind::Text | crate::types::TypeKind::Enum | crate::types::TypeKind::Set
    )
    .then(|| source.ty.collation.name().to_owned())
}

/// The enclosing query's row, as a scope a join condition can be read in.
///
/// A join's ON is evaluated while the relation is being built, before the
/// query has anything to hand a correlated expression, so the outer row cannot
/// arrive the way a WHERE's does — `select` leaves it on the session for the
/// duration instead, and this is where the two joins pick it up.
///
/// Empty slices rather than an `Option` so the callers append unconditionally:
/// a scope that is only widened on one branch is a scope that gets widened for
/// the resolution and not for the evaluation, which is exactly the split #0230
/// was filed for.
///
/// One definition for both joins so a mutation to it neuters both at once.
fn correlated_scope(session: &Session) -> (&[ColumnRef], &[Value]) {
    match &session.lateral {
        Some(outer) => (&outer.schema, &outer.values),
        None => (&[], &[]),
    }
}

/// One join's row with the enclosing query's values behind it.
///
/// Borrowed where there is no correlation, which is nearly every join: this
/// runs once per pair of rows considered, and copying the pair to append
/// nothing to it would make every join in the suite pay for the one shape that
/// needs it.
///
/// Behind and not in front, matching the order [`correlated_scope`]'s schema
/// is appended in — the two have to line up index for index or a name resolves
/// to the wrong value rather than to none at all.
fn with_outer<'a>(row: &'a [Value], outer: &[Value]) -> std::borrow::Cow<'a, [Value]> {
    if outer.is_empty() {
        return std::borrow::Cow::Borrowed(row);
    }
    let mut widened = row.to_vec();
    widened.extend_from_slice(outer);
    std::borrow::Cow::Owned(widened)
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
        let column = name::dotted(name);
        let bare = name::ColumnParts::bare(&column);
        let (Some(l), Some(r)) = (left.index_of(&bare), right.index_of(&bare)) else {
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

impl crate::expr::Executor for Engine {
    fn rows(
        &self,
        query: &sqlparser::ast::Query,
        session: &Session,
        outer: Option<crate::expr::OuterRow>,
    ) -> EngineResult<Vec<Vec<Value>>> {
        Ok(self.query_with_outer(session, query, outer.as_ref())?.rows)
    }

    fn state_image(&self, save: bool, path: &str) -> EngineResult<Value> {
        let path = std::path::Path::new(path);
        if save {
            self.save_image(path)?;
        } else {
            self.load_image(path)?;
        }
        // A row rather than an OK packet, because it is a function: a client
        // that selects it wants something back, and 1 is what MySQL's own
        // side-effecting functions return.
        Ok(Value::Int(1))
    }

    fn next_random(&self, seed: Option<i64>) -> f64 {
        // A seed restarts the sequence, as MySQL's RAND(N) does; without one
        // the counter simply advances. xorshift64* keeps this to a few lines
        // and needs no dependency — the numbers only have to be spread out,
        // not unpredictable.
        if let Some(n) = seed {
            self.randoms
                .store(n as u64, std::sync::atomic::Ordering::Relaxed);
        }
        let counter = self
            .randoms
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // splitmix64. The counter advances by one, so the mixer has to spread
        // adjacent inputs across the whole range: a bare xorshift left them
        // nearly equal, and `ORDER BY RAND()` then returned rows in scan order
        // while `WHERE RAND() < 0.5` matched every row or none.
        let mut z = counter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;

        // 53 bits is what an f64 holds exactly, so the division is lossless.
        (z >> 11) as f64 / (1u64 << 53) as f64
    }

    fn next_uuid(&self) -> String {
        let n = self
            .uuids
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Shaped like a version-4 UUID so anything that validates the format
        // accepts it, but the body is a counter rather than randomness.
        //
        // The counter goes in the *leading* field, as MySQL's time-low does.
        // With it at the end every value shared a 24-character prefix, and the
        // migrations derive ids with `SUBSTRING(REPLACE(UUID(), '-', ''), 1, 21)`
        // — every one of which collided.
        let low = n as u32;
        let high = (n >> 32) as u16;
        format!("{low:08x}-{high:04x}-4000-8000-{n:012x}")
    }

    fn named_lock(&self, op: crate::expr::LockOp, name: &str, connection_id: u32) -> Value {
        use crate::expr::LockOp;

        // A poisoned lock table would only happen if a panic unwound while it
        // was held, and there is nothing to recover — take it either way.
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        let holder = locks.get(name).copied();

        match op {
            LockOp::Acquire => {
                // MySQL lets a connection re-take a lock it already holds. A
                // lock held elsewhere returns 0 rather than blocking: this
                // engine is a single test process, so waiting for a holder
                // that will never release is worse than saying no.
                match holder {
                    Some(id) if id != connection_id => Value::Int(0),
                    _ => {
                        locks.insert(name.to_owned(), connection_id);
                        Value::Int(1)
                    }
                }
            }
            LockOp::Release => match holder {
                Some(id) if id == connection_id => {
                    locks.remove(name);
                    Value::Int(1)
                }
                Some(_) => Value::Int(0),
                None => Value::Null,
            },
            LockOp::IsFree => Value::Int(i64::from(holder.is_none())),
            LockOp::UsedBy => holder.map_or(Value::Null, |id| Value::Int(i64::from(id))),
            LockOp::ReleaseAll => {
                let before = locks.len();
                locks.retain(|_, id| *id != connection_id);
                Value::Int((before - locks.len()) as i64)
            }
        }
    }
}

/// What a multi-statement command produced: one outcome per statement that
/// ran, and the error that stopped the rest.
pub struct Executed {
    pub outcomes: Vec<Outcome>,
    /// How many conditions each outcome's statement raised, in the same order.
    ///
    /// Parallel to `outcomes` rather than read off the session at the end: the
    /// diagnostics area holds only the *last* statement's conditions by then,
    /// and a batch shaped against it would tell the client that every
    /// statement in a migration raised whatever the last one did.
    pub warnings: Vec<u16>,
    pub error: Option<EngineError>,
}

impl Executed {
    fn of(outcomes: Vec<Outcome>, warnings: Vec<u16>) -> Self {
        // The two lists are read by zipping them, which silently drops the
        // tail of the longer one -- so a statement whose count went missing
        // would take every response after it with it, and look like a batch
        // that ran short rather than like a bug here.
        debug_assert_eq!(outcomes.len(), warnings.len(), "one count per outcome");
        Self {
            outcomes,
            warnings,
            error: None,
        }
    }

    fn failed(outcomes: Vec<Outcome>, warnings: Vec<u16>, error: EngineError) -> Self {
        debug_assert_eq!(outcomes.len(), warnings.len(), "one count per outcome");
        Self {
            outcomes,
            warnings,
            error: Some(error),
        }
    }
}

/// The rows a query folds over, and the two schemas needed to read them.
///
/// `refs` is what a name resolves against — this query's columns plus any the
/// enclosing one contributes — while `columns` is the relation itself, which is
/// where a column's declared type lives.
struct Input<'a> {
    refs: &'a [ColumnRef],
    columns: &'a [crate::relation::RelColumn],
    matching: &'a [&'a Vec<Value>],
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
            // `sort_cmp`, not `compare`: MySQL sorts an ENUM by its declared
            // order and compares one by its member, and the two disagree.
            (false, false) => left.sort_cmp(right).unwrap_or(std::cmp::Ordering::Equal),
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

/// A table a multi-table DELETE removes rows from, as the statement wrote it.
///
/// The database is kept rather than dropped, which is the whole of #0153: both
/// readers took the last part of the name and looked at nothing in front of it,
/// so `DELETE nodb.A FROM A JOIN B ON A.id = B.id` deleted from the current
/// schema where 8.3 raises 1109 — the same shape as the column reference #0098
/// fixed, on a write, and answering rather than refusing.
struct DeleteTarget {
    /// The database written in front of the table, `None` where none was.
    ///
    /// Not defaulted to the session's here: an absent database and one spelled
    /// out are the same thing against an unaliased FROM item and *different*
    /// things against an aliased one, so the fallback belongs where the
    /// comparison is rather than at the point of reading.
    database: Option<String>,
    name: String,
}

impl DeleteTarget {
    /// A target from the `DELETE a, b FROM ...` spelling.
    ///
    /// Read through [`name::qualifier`] rather than by splitting a rendering,
    /// so a table spelled `` `my.table` `` is one part carrying a dot. A name
    /// with more parts than that has no meaning in the grammar — 8.3 answers
    /// `DELETE a.b.c FROM A JOIN B ON A.id = B.id` with 1064 — and this is the
    /// same code rather than a silent read of the last two parts.
    fn written(name: &ObjectName) -> EngineResult<Self> {
        let (database, table) = name::qualifier(name).ok_or_else(|| {
            EngineError::parse(format!(
                "a DELETE target is written `table` or `database.table`, not `{}`",
                name::dotted(name)
            ))
        })?;
        Ok(Self {
            database,
            name: table,
        })
    }

    /// Whether two targets name the same table, for the 1066 that a repeated
    /// one raises.
    ///
    /// `current` is the session's database, which an absent qualifier means:
    /// `DELETE db.A, A FROM ...` is 1066 on 8.3 while the session is in `db`,
    /// and not two targets.
    fn same_as(&self, other: &Self, current: &str) -> bool {
        let mine = self.database.as_deref().unwrap_or(current);
        let theirs = other.database.as_deref().unwrap_or(current);
        self.name.eq_ignore_ascii_case(&other.name) && mine.eq_ignore_ascii_case(theirs)
    }
}

/// The target a `DELETE FROM a, b USING ...` writes before its USING clause.
///
/// Read exactly as the other spelling reads one, database and all. The parser
/// accepts a *table reference* here, which is wider than MySQL's grammar: the
/// targets before `USING` are names, and the relation — with its aliases — is
/// what the `USING` clause describes. 8.3 answers `DELETE FROM A a USING A a
/// JOIN B ON a.id = B.id` with 1064, and with the same 1064 when the database
/// is spelled correctly in front of it.
///
/// Refused rather than read, because reading it is how the qualifier went
/// missing: taking the alias as the whole target discarded the database with
/// it, so `DELETE FROM nodb.A a USING A a JOIN B ON a.id = B.id` deleted from
/// the current schema and reported the rows affected — the bug this module's
/// `DeleteTarget` exists to stop, surviving in the one spelling that never
/// reached the comparison.
fn target_written(item: &TableWithJoins) -> EngineResult<DeleteTarget> {
    let TableFactor::Table { name, alias, .. } = &item.relation else {
        return Err(unsupported("this DELETE target"));
    };
    // A join here would be naming a target and describing the relation at
    // once; USING is where the relation belongs.
    if !item.joins.is_empty() {
        return Err(unsupported(
            "a join among the targets of a DELETE ... USING",
        ));
    }
    if let Some(alias) = alias {
        return Err(EngineError::parse(format!(
            "a DELETE ... USING names its targets as tables and cannot rename one: \
             drop the alias `{}`, which belongs in the USING clause",
            alias.name.value
        )));
    }
    DeleteTarget::written(name)
}

/// The narrowest type that can hold what either branch of a set operation
/// produces.
///
/// MySQL's own aggregation, checked against 8.3 pair by pair: two integers
/// give the wider integer, and two of the *same* width with different
/// signedness give the next size up, because neither can hold the other's
/// range. A decimal anywhere among numbers gives a decimal, an approximate one
/// gives a double, and a date beside a datetime gives a datetime. Anything
/// else falls back to a string, which is what MySQL does for `SMALLINT UNION
/// VARCHAR`.
///
/// Getting this wrong is not cosmetic: the binary protocol encodes by the
/// declared code, so a union announced as the narrower of its two branches
/// sends too few bytes for the other one's values.
fn widen(a: ColumnType, a_unsigned: bool, b: ColumnType, b_unsigned: bool) -> ColumnType {
    // Integers, in the order they can hold each other.
    let integer = |t: ColumnType| match t {
        ColumnType::Tiny => Some(1u8),
        ColumnType::Short | ColumnType::Year => Some(2),
        ColumnType::Int24 => Some(3),
        ColumnType::Long => Some(4),
        ColumnType::LongLong => Some(5),
        _ => None,
    };
    let from_rank = |r: u8| match r {
        1 => ColumnType::Tiny,
        2 => ColumnType::Short,
        3 => ColumnType::Int24,
        4 => ColumnType::Long,
        _ => ColumnType::LongLong,
    };
    let approximate = |t: ColumnType| matches!(t, ColumnType::Float | ColumnType::Double);
    let exact = |t: ColumnType| matches!(t, ColumnType::Decimal | ColumnType::NewDecimal);
    let temporal = |t: ColumnType| {
        matches!(
            t,
            ColumnType::Date | ColumnType::Time | ColumnType::DateTime | ColumnType::Timestamp
        )
    };

    // The integers first, because two of them can be the same code and still
    // need widening: an `INT` and an `INT UNSIGNED` are both `LONG`, and an
    // early "same code, same answer" return would miss exactly that.
    if let (Some(x), Some(y)) = (integer(a), integer(b)) {
        // Whichever is unsigned needs one size more than itself to be held
        // signed — an `INT UNSIGNED` reaches 4294967295, which only a
        // `BIGINT` covers — and the answer has to cover the other side too.
        //
        // Not only at equal widths, which is what this asked before:
        // `INT UNSIGNED UNION SMALLINT` came back a signed `LONG` and handed
        // a binary client -1 for 4294967295. And a `SMALLINT UNSIGNED` beside
        // an `INT` needs nothing extra, because the `INT` already covers it.
        let needed = match (a_unsigned, b_unsigned) {
            (true, false) => x.saturating_add(1).max(y),
            (false, true) => y.saturating_add(1).max(x),
            _ => x.max(y),
        };
        // Past `BIGINT` there is no integer left: MySQL answers a DECIMAL,
        // because nothing else holds both a u64 and a negative.
        if needed > 5 {
            return ColumnType::NewDecimal;
        }
        return from_rank(needed);
    }
    if a == b {
        return a;
    }
    let numeric = |t: ColumnType| integer(t).is_some() || approximate(t) || exact(t);
    if numeric(a) && numeric(b) {
        return if approximate(a) || approximate(b) {
            // A float beside an integer is still a float — MySQL types
            // `COALESCE(f, 0)` and `FLOAT UNION INT` FLOAT, and shows the six
            // digits a float has. Only a double or a decimal alongside makes
            // it a double; the decimal because a float cannot hold one.
            if a == ColumnType::Double || b == ColumnType::Double || exact(a) || exact(b) {
                ColumnType::Double
            } else {
                ColumnType::Float
            }
        } else {
            ColumnType::NewDecimal
        };
    }
    if temporal(a) && temporal(b) {
        return if matches!(a, ColumnType::DateTime | ColumnType::Timestamp)
            || matches!(b, ColumnType::DateTime | ColumnType::Timestamp)
        {
            ColumnType::DateTime
        } else {
            ColumnType::VarString
        };
    }
    ColumnType::VarString
}

/// MySQL refuses a join in which two sources answer to the same name, because
/// a qualified reference could not say which one it meant. Without this check
/// `source_index` silently resolves to the first, so a write can land on a
/// table the statement never named.
fn reject_duplicate_sources(relation: &Relation) -> EngineResult<()> {
    for (i, name) in relation.sources.iter().enumerate() {
        if relation.sources[..i]
            .iter()
            .any(|earlier| earlier.eq_ignore_ascii_case(name))
        {
            return Err(EngineError::new(
                1066,
                "42000",
                format!("Not unique table/alias: '{name}'"),
            ));
        }
    }
    Ok(())
}

/// One value of an inserted row: still an expression from a VALUES list, or
/// already computed by the SELECT of an `INSERT ... SELECT`.
enum SourceValue {
    // Boxed because an `Expr` is an order of magnitude larger than a `Value`,
    // and every value of every inserted row is one of these.
    Expr(Box<Expr>),
    Value(Value),
}

/// Whether a value list leaves this column to work itself out.
///
/// DEFAULT and NULL are the only two things MySQL accepts in a generated
/// column's position, and both mean "compute it"; anything else is 3105.
fn writes_nothing(source: &SourceValue) -> bool {
    match source {
        SourceValue::Expr(e) => match e.as_ref() {
            Expr::Value(v) => matches!(v.value, sqlparser::ast::Value::Null),
            other => is_default_keyword(other),
        },
        SourceValue::Value(v) => v.is_null(),
    }
}

/// Names the file and what was being done to it.
///
/// The bare `io::Error` says "permission denied" without saying to what, and a
/// bake step that cannot write its image should not need a strace to diagnose.
fn file_error(path: &std::path::Path, action: &str, e: &std::io::Error) -> EngineError {
    EngineError::new(
        1105,
        "HY000",
        format!("cannot {action} state image `{}`: {e}", path.display()),
    )
}

// Whether this thread is inside a writer's copy-compute-commit cycle. A
// statement runs to completion on one thread — nothing in the engine awaits —
// so a thread-local is exactly the scope of one statement.
thread_local! {
    static WRITING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Holds the writer gate, and the flag that makes re-entering it detectable.
struct WriteGate<'a> {
    _guard: std::sync::MutexGuard<'a, ()>,
}

impl Drop for WriteGate<'_> {
    fn drop(&mut self) {
        WRITING.with(|held| held.set(false));
    }
}

/// A row before and after an UPDATE, which is what a cascade needs to match
/// children against the old key and rewrite them with the new one.
type MovedRow = (Vec<Value>, Vec<Value>);

/// Applies each dependent table's ON UPDATE action after a parent key changes.
///
/// Without this an UPDATE of a parent key leaves children pointing at a value
/// that no longer exists — a dangling reference the engine would then reject on
/// the next INSERT of the same value.
fn cascade_update(
    database: &mut Database,
    parent_table: &str,
    changed: &[MovedRow],
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

/// Fills any AUTO_INCREMENT column the row left empty, and advances the
/// counter past one it filled in itself.
///
/// Shared by INSERT and by `CREATE TABLE ... AS SELECT`, because a counter
/// advanced on one path and not the other leaves a table whose next insert
/// collides in its own primary key — which is what CTAS did before it came
/// through here, every row taking the type's implicit default of zero.
///
/// Answers the value it generated, if any. Recorded rather than reported: an
/// `ON DUPLICATE KEY UPDATE` that updates instead of inserting must not hand
/// back an id for a row that does not exist, and MySQL consumes the counter
/// value either way, which is why it advances here regardless.
fn assign_auto_increment(table: &mut Table, row: &mut [Value]) -> EngineResult<Option<u64>> {
    // The counter lives on the table, so the columns are collected first and
    // the row walked by index rather than the two being iterated together.
    let auto_increment: Vec<usize> = table
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.auto_increment)
        .map(|(i, _)| i)
        .collect();
    let mut generated = None;
    for slot in auto_increment {
        if row[slot].is_null() {
            // Through `coerce` like any other value, because a generated id is
            // subject to the column's width too. The doc on `column_type`
            // rests on nothing outliving the declared code, and a `TINYINT
            // AUTO_INCREMENT` counting past 127 would otherwise be sent as one
            // byte and read back negative.
            row[slot] = table.columns[slot].ty.store(
                Value::Int(table.next_auto_increment),
                &table.columns[slot].name,
            )?;
            generated = Some(table.next_auto_increment as u64);
            table.next_auto_increment += 1;
        } else if let Some(explicit) = row[slot].as_int() {
            // An explicit value moves the counter past it, as MySQL does.
            // Without this, a fixture that inserts explicit ids collides with
            // generated ones later.
            table.next_auto_increment = table.next_auto_increment.max(explicit + 1);
        }
    }
    Ok(generated)
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
                // Coerced like any other value written to the column: an
                // expression default that produces text for a JSON column has
                // to arrive as a document, not as a string of one.
                column.ty.store(expr::eval(&parsed, ctx)?, &column.name)?
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

/// How many rows a query's LIMIT can possibly want, offset included.
///
/// `None` where there is no LIMIT, and where the clause is one this engine
/// does not read — a producer that stops early on a number it guessed would
/// answer fewer rows than the statement asked for, so not knowing has to mean
/// "no bound" rather than zero.
///
/// The offset is added rather than ignored: `LIMIT 3 OFFSET 2` needs five rows
/// produced before it can keep three, and 8.3 produces exactly five —
/// `... UNION ALL SELECT n + 1 FROM t LIMIT 3 OFFSET 2` answers 3, 4, 5 rather
/// than running away.
fn limit_reach(session: &Session, query: &Query) -> EngineResult<Option<usize>> {
    use sqlparser::ast::LimitClause;
    // LIMIT and OFFSET are constants or parameters, never subqueries — the
    // same argument `apply_limit` makes, and the same context it evaluates in.
    let ctx = Context::rowless(session);
    let (limit, offset) = match &query.limit_clause {
        Some(LimitClause::LimitOffset { limit, offset, .. }) => (
            limit.as_ref().map(|e| expr::eval(e, &ctx)).transpose()?,
            offset
                .as_ref()
                .map(|o| expr::eval(&o.value, &ctx))
                .transpose()?,
        ),
        Some(LimitClause::OffsetCommaLimit { offset, limit }) => (
            Some(expr::eval(limit, &ctx)?),
            Some(expr::eval(offset, &ctx)?),
        ),
        None => return Ok(None),
    };
    let Some(limit) = limit.and_then(|v| v.as_int()) else {
        return Ok(None);
    };
    let skipped = offset.and_then(|v| v.as_int()).unwrap_or(0).max(0) as usize;
    Ok(Some(skipped.saturating_add(limit.max(0) as usize)))
}

/// Applies a CTE's `WITH x (a, b) AS ...` column list, which renames the
/// columns of its result.
fn rename_cte_columns(
    relation: &mut Relation,
    cte: &sqlparser::ast::Cte,
    name: &str,
) -> EngineResult<()> {
    if cte.alias.columns.is_empty() {
        return Ok(());
    }
    if cte.alias.columns.len() != relation.columns.len() {
        return Err(EngineError::new(
            1353,
            "HY000",
            format!(
                "In definition of view, derived table or common table expression, \
                 SELECT and column names list have different column counts: '{name}'"
            ),
        ));
    }
    for (column, alias) in relation.columns.iter_mut().zip(&cte.alias.columns) {
        column.name = alias.name.value.clone();
    }
    Ok(())
}

/// The comparison key for a whole row, under the server's collation.
fn row_key(row: &[Value]) -> Vec<String> {
    row.iter()
        .map(|v| {
            if v.is_null() {
                // NULL is its own value and must not collide with the empty
                // string.
                "\u{0}NULL".to_owned()
            } else {
                v.collation_key()
            }
        })
        .collect()
}

/// How many times each row appears, for the multiset behaviour of
/// INTERSECT ALL and EXCEPT ALL.
fn row_counts(rows: &[Vec<Value>]) -> HashMap<Vec<String>, usize> {
    let mut counts: HashMap<Vec<String>, usize> = HashMap::new();
    for row in rows {
        *counts.entry(row_key(row)).or_insert(0) += 1;
    }
    counts
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
        let key = row_key(&row);
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

/// The sum of values that are all exact, and the scale it is counted at.
///
/// `None` as soon as one of them is approximate — a `DOUBLE` column, or a
/// string with no decimal reading — because MySQL only promises a scale for
/// the exact types, and a `SUM` over a `DOUBLE` is a `DOUBLE`.
///
/// The widest scale present, so a column and a literal beside it agree: MySQL
/// takes the larger of the two rather than the first it sees.
fn exact_total(values: &[&Value]) -> Option<(i128, usize)> {
    let scale = values
        .iter()
        .map(|v| match exact_operand(v)?.as_ref() {
            Value::Int(_) | Value::Unsigned(_) => Some(0),
            Value::Decimal(text, _) => Some(crate::decimal::scale_of(text)),
            _ => None,
        })
        .try_fold(0usize, |widest, scale| Some(widest.max(scale?)))?;

    let mut total: i128 = 0;
    for value in values {
        let units = match exact_operand(value)?.as_ref() {
            // Checked, because `scale` comes from a value's own text and
            // nothing bounds how many digits that has.
            Value::Int(n) => i128::from(*n).checked_mul(10i128.checked_pow(scale as u32)?)?,
            Value::Unsigned(n) => i128::from(*n).checked_mul(10i128.checked_pow(scale as u32)?)?,
            Value::Decimal(text, _) => crate::decimal::units(text, scale)?,
            _ => return None,
        };
        total = total.checked_add(units)?;
    }
    Some((total, scale))
}

/// The scale a total is *shown* at: the widest its addends are shown at.
///
/// The same "widest wins" rule [`exact_total`] applies to the digits, over the
/// declared scale instead. The two are one number until a quotient is summed
/// and then are not — `SUM(a/3)` over a `DECIMAL(10,2)` totals nine-place
/// values and shows six of them on 8.3 — which is the whole of #0118 seen
/// through an aggregate.
///
/// A value with no exact reading contributes nothing rather than refusing: the
/// caller has already established that every addend has one.
fn shown_scale(values: &[&Value]) -> usize {
    values
        .iter()
        .filter_map(|v| exact_operand(v)?.display_scale())
        .max()
        .unwrap_or(0)
}

/// The inexact total of everything a `SUM` or an `AVG` no exact type covers
/// was given.
///
/// Through `as_number`, which is MySQL's leading-number rule, and not through
/// `as_float`, which parses the whole string or nothing. The two disagree on
/// exactly the values this path exists for: over a TEXT column holding
/// `'12abc'` and `'3.5x'`, MySQL answers `SUM` 15.5 and `AVG` 7.75 — the same
/// numbers `t + 0` reads out of each row — where dropping what would not parse
/// answered 0 and made one row and its total disagree inside one statement.
/// `as_float` has no case for a binary string at all, so `SUM` over a BLOB was
/// 0 whatever it held; MySQL reads one the same way it reads text.
///
/// Every value counts, so there is nothing to filter: MySQL's arithmetic has
/// no "not a number" case and `as_number` answers 0 for text that has no
/// leading number. NULLs are dropped before this is called.
///
/// That is also what fixed `SUM` over a column with nothing numeric in it
/// answering `-0` where MySQL answers 0: `Iterator::sum` for `f64` starts at
/// `-0.0`, which used to be the whole total once the filter had dropped every
/// value. Every value contributing means there is always an addend and the
/// seed cannot survive, so the explicit `0.0` below no longer decides
/// anything — it is kept so that re-introducing a filter here cannot quietly
/// bring the `-0` back, not because it is what makes the answer right.
fn approximate_total(values: &[&Value]) -> f64 {
    values.iter().fold(0.0, |total, v| total + v.as_number())
}

/// One aggregated value as the exact number it counts as, borrowed where it
/// already is one.
///
/// A temporal is exact too, at its packed number: MySQL answers `SUM(d)`
/// 20240105 and `AVG(d)` 20240105.0000 for a DATE of 2024-01-05, both
/// NEWDECIMAL.
///
/// Borrowed rather than promoted into a vector, because this runs per row and
/// per group: building one cost an allocation and a `String` clone for every
/// value in every `SUM` and `AVG`, to carry a case most columns do not have.
/// The same function of the same text in both passes, so the scale and the
/// units still cannot read one value two ways.
fn exact_operand(value: &Value) -> Option<std::borrow::Cow<'_, Value>> {
    match value {
        // An unsigned value is exact too, and it has to be read here rather
        // than left to `approximate_total`: `SUM(bu)` over a BIGINT UNSIGNED
        // holding 18446744073709551615 is that number on 8.3, where the `f64`
        // path renders 18446744073709552000.
        Value::Int(_) | Value::Decimal(..) | Value::Unsigned(_) => {
            Some(std::borrow::Cow::Borrowed(value))
        }
        Value::Temporal(_) => value.temporal_number().map(std::borrow::Cow::Owned),
        _ => None,
    }
}

/// The name of the hidden column a folded aggregate is bound to.
///
/// A NUL prefix keeps it out of any namespace a query could reference.
fn folded_name(index: usize) -> String {
    format!("\u{0}agg{index}")
}

/// The name of the hidden column a projected alias is bound to.
///
/// Shares the NUL prefix with `folded_name` above, and shares its reason: an
/// alias reaches HAVING and ORDER BY as a *value*, and there is no spelling of
/// one that survives the trip through the syntax tree. A quoted string loses a
/// temporal's packed number and an ENUM's ordinal, so `HAVING e = 1` compared
/// the member against a number and matched no group at all; binary data would
/// not survive being written as text either.
fn alias_name(index: usize) -> String {
    format!("\u{0}alias{index}")
}

/// The alias and expression of each projected item.
fn projected_items(projection: &[SelectItem]) -> EngineResult<Vec<(String, Expr)>> {
    projection
        .iter()
        .map(|item| match item {
            SelectItem::UnnamedExpr(e) => Ok((render_expr(e), e.clone())),
            SelectItem::ExprWithAlias { expr, alias } => Ok((name::alias(alias), expr.clone())),
            other => Err(unsupported(format!(
                "select item `{other}` in a grouped query"
            ))),
        })
        .collect()
}

/// The 1-based position an expression names, if it is a bare integer.
fn ordinal_of(e: &Expr) -> Option<usize> {
    written_position(e).and_then(|(position, _)| (position > 0).then_some(position))
}

/// The position a bare number literal names, and the digits it was written
/// with.
///
/// Zero is a position too, which is why this exists beside `ordinal_of`: 8.3
/// answers `ORDER BY 0` with `Unknown column '0'` rather than sorting on the
/// constant, so the caller needs to see the number before it can decide that
/// nothing answers to it. The digits go back because the message quotes them
/// rather than the parsed value.
///
/// Only a literal, and only an integer one. Parentheses and unary plus are
/// peeled because 8.3 peels them: `ORDER BY (2)`, `ORDER BY +2` and the doubled
/// `ORDER BY + +2` all name the second output column there. Arithmetic is not a
/// position — `ORDER BY 1 + 1` and `ORDER BY 1.0` are constants on 8.3 and
/// leave the rows in the order they arrived — and neither is a *minus*:
/// `ORDER BY -1` is a constant there and sorts nothing, which is why this peels
/// with `peeled` rather than with anything that would take a sign.
///
/// The digits go back without the wrapper, which is also 8.3: a position out of
/// range under a plus is `Unknown column '2' in 'order clause'`.
fn written_position(e: &Expr) -> Option<(usize, &str)> {
    match peeled(e) {
        Expr::Value(v) => match &v.value {
            sqlparser::ast::Value::Number(n, _) if n.bytes().all(|b| b.is_ascii_digit()) => {
                // A run of digits too long for a `usize` is still a position,
                // and one nothing can answer to. Saturating keeps it out of
                // range rather than letting a parse failure read it as a
                // constant.
                Some((n.parse::<usize>().unwrap_or(usize::MAX), n.as_str()))
            }
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

/// One column a select list projects: the name it answers to, the expression
/// behind it, and which relation column it *is*, when it is one.
///
/// The slot is the identity ORDER BY compares candidates on. Two items that
/// are the same column are not ambiguous however they are spelled — `SELECT
/// t.b, b FROM t ORDER BY b` answers on 8.3 — and two that are different
/// columns are, which no comparison of the written SQL could tell apart.
struct OutItem {
    name: String,
    expr: Expr,
    slot: Option<usize>,
}

/// One resolved ORDER BY item: what to sort on, and which way.
struct SortKey {
    expr: Expr,
    descending: bool,
}

/// Every column a select list projects, with `*` expanded.
///
/// ORDER BY counts positions over what the query *returns*, so a `*` has to be
/// expanded before a position can be resolved against it: `SELECT * FROM t
/// ORDER BY 3` sorts on the third column of the table on 8.3, and `ORDER BY 4`
/// over a three-column table is 1054.
///
/// A wildcard-expanded item carries its relation index directly rather than
/// letting the identifier it is written as be looked up again, because a
/// lookup stops at the first column of that name and a self-join has two.
fn output_items(
    projection: &[SelectItem],
    relation: &Relation,
    refs: &[ColumnRef],
    coalesced: &[String],
) -> EngineResult<Vec<OutItem>> {
    use sqlparser::ast::Ident;

    let mut out: Vec<OutItem> = Vec::new();
    for item in projection {
        let qualifier = match item {
            SelectItem::Wildcard(_) => None,
            // Refused rather than expanded to nothing where the qualifier
            // names no FROM item: `resolve_field_list` has already settled
            // that for every path that runs one, and a path that reached here
            // without it would otherwise answer a wildcard with no columns.
            SelectItem::QualifiedWildcard(kind, _) => Some(
                wildcard_qualifier(kind)
                    .ok_or_else(|| EngineError::bad_table(&wildcard_written(kind)))?,
            ),
            SelectItem::UnnamedExpr(e) => {
                out.push(OutItem {
                    name: render_expr(e),
                    expr: e.clone(),
                    slot: column_slot(refs, e),
                });
                continue;
            }
            SelectItem::ExprWithAlias { expr, alias } => {
                out.push(OutItem {
                    name: name::alias(alias),
                    expr: expr.clone(),
                    slot: column_slot(refs, expr),
                });
                continue;
            }
            other => return Err(unsupported(format!("select item `{other}`"))),
        };
        // Where this wildcard's own columns start, because the coalescing
        // below may only look at them. Searching everything appended so far
        // let a `USING` column bind to an *aliased* item that merely shares
        // its name: `SELECT LENGTH(a.s) AS id, a.*, bb.* FROM a JOIN bb USING
        // (id) ORDER BY 2` then sorted on the LENGTH rather than on `a.id`.
        let expansion = out.len();
        for (index, column) in relation.columns.iter().enumerate() {
            if qualifier.as_ref().is_some_and(|(database, table)| {
                !column.reference().in_source(database.as_deref(), table)
            }) {
                continue;
            }
            let written = if column.table.is_empty() {
                Expr::Identifier(Ident::new(column.name.clone()))
            } else {
                Expr::CompoundIdentifier(vec![
                    Ident::new(column.table.clone()),
                    Ident::new(column.name.clone()),
                ])
            };
            // A column `USING` joined on is one column in MySQL, which
            // returns it once; this engine's relation keeps both sides, so
            // `SELECT * FROM a JOIN b USING (id)` puts two columns called
            // `id` in front of the clause and `ORDER BY id` became 1052 —
            // a working query refused. The second one resolves to the first,
            // so the name has one answer while the position count still
            // matches the columns this engine actually returns.
            //
            // Only within this wildcard's own expansion, which is not merely
            // about not binding to an unrelated aliased item: 8.3 coalesces a
            // USING column for the bare `*` and *not* across two qualified
            // ones. Measured, because it has been argued both ways — `SELECT
            // t.*, u.* FROM t JOIN u USING (id) ORDER BY id` is `Column 'id'
            // in order clause is ambiguous` there, in either order of the two
            // wildcards, while `WHERE id = 1` beside it answers because that
            // clause reads the FROM and not the output.
            let same = coalesced
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&column.name))
                .then(|| {
                    out[expansion..]
                        .iter()
                        .find(|o| o.name.eq_ignore_ascii_case(&column.name))
                })
                .flatten();
            let (expr, slot) = match same {
                Some(first) => (first.expr.clone(), first.slot),
                None => (written, Some(index)),
            };
            out.push(OutItem {
                name: column.name.clone(),
                expr,
                slot,
            });
        }
    }
    Ok(out)
}

/// Every column name a `USING` clause in this FROM joins on.
///
/// MySQL coalesces such a column into one; this engine's relation keeps both,
/// so the names it puts in front of a clause have to be read with that in
/// mind. See `output_items`.
fn coalesced_columns(from: &[TableWithJoins]) -> Vec<String> {
    let mut names = Vec::new();
    for item in from {
        for join in &item.joins {
            if let Some(sqlparser::ast::JoinConstraint::Using(using)) =
                join_constraint(&join.join_operator)
            {
                names.extend(using.iter().map(name::dotted));
            }
        }
    }
    names
}

/// Where a bare name written in an ORDER BY lands in the select list.
///
/// A left-to-right scan, and the direction it walks is load-bearing rather
/// than incidental. An item that is *not* a column of the relation wins as
/// soon as it is reached and nothing after it is looked at; two items that are
/// different columns are ambiguous; two that are the same column are not.
/// Measured all three ways on 8.3, over a table with columns `id` and `s`:
///
/// ```sql
/// SELECT id AS b, s AS b, 1 AS b FROM t ORDER BY b   -- 1052, before the `1`
/// SELECT 1 AS b, id AS b, s AS b FROM t ORDER BY b   -- answers, sorts on 1
/// SELECT id AS b, LENGTH(s) AS b FROM t ORDER BY b   -- sorts on LENGTH(s)
/// SELECT b, t.b FROM t ORDER BY b                    -- answers, one column
/// ```
///
/// The third is the one that rules out the reading this first had, that a
/// column wins over an expression sharing its name: it sorts on the expression
/// from either position. Both readings agree on the rows unless the sort is
/// written `DESC`, which is how the wrong one survived a first measurement.
fn select_list_slot(name: &str, items: &[OutItem]) -> EngineResult<Option<usize>> {
    let mut column: Option<usize> = None;
    for (index, item) in items.iter().enumerate() {
        if !item.name.eq_ignore_ascii_case(name) {
            continue;
        }
        let Some(slot) = item.slot else {
            return Ok(Some(index));
        };
        match column {
            Some(first) if items[first].slot != Some(slot) => {
                return Err(EngineError::ambiguous(name, "order clause"))
            }
            Some(_) => {}
            None => column = Some(index),
        }
    }
    Ok(column)
}

/// Resolves one ORDER BY item against the select list, by the order 8.3
/// resolves in.
///
/// Three readings, and the *shape* of the item picks which one applies rather
/// than whatever happens to resolve:
///
/// - A bare integer is a position in the select list and never anything else.
///   `SELECT JSON_ARRAY(x) FROM t ORDER BY 1` sorts on the document 8.3
///   returns and not on `x`, which is the whole of #0090; this engine
///   evaluated the number against the source row, where it is the column in
///   that position.
/// - A bare name sees the select list *first*: `SELECT id AS b FROM t ORDER BY
///   b` sorts on `id` on 8.3 even where `t` has a column called `b`, which is
///   #0144.
/// - A name inside a larger expression sees the FROM clause first, and the
///   select list only where the relation has no such column. Both directions
///   measured on 8.3 over a table with a column `b`: `ORDER BY b` sorts on the
///   alias and `ORDER BY b + 1` sorts on the column, while `ORDER BY zz + 1`
///   with no column `zz` sorts on the alias. So this is a precedence and not a
///   choice of one clause over the other — getting it backwards either way
///   breaks half of the measured cases.
///
/// A qualifier never reaches an alias, at either level: `SELECT id AS b FROM t
/// ORDER BY t.b` asks for a column of `t` on 8.3, and an alias is not one.
///
/// `bind` says what a resolved select-list position becomes, because the two
/// callers need different things from it. The ungrouped path sorts source rows
/// and takes the expression back; the grouped path has already computed one
/// value per group and binds that instead — see `alias_name` for why a value
/// cannot be respelled as SQL.
fn resolve_order_expr(
    e: &Expr,
    items: &[OutItem],
    refs: &[ColumnRef],
    bind: &dyn Fn(usize) -> Expr,
) -> EngineResult<Expr> {
    if let Some((position, written)) = written_position(e) {
        if position == 0 || position > items.len() {
            return Err(EngineError::unknown_column(written, "order clause"));
        }
        return Ok(bind(position - 1));
    }

    if let Expr::Identifier(ident) = peeled(e) {
        if let Some(index) = select_list_slot(&ident.value, items)? {
            return Ok(bind(index));
        }
        return Ok(e.clone());
    }

    // Inside an expression the relation wins, so only a name it cannot answer
    // falls back to the select list.
    let ambiguous = std::cell::RefCell::new(None);
    let resolved = substitute_names(e, &|written| {
        if column_slot(refs, &Expr::Identifier(sqlparser::ast::Ident::new(written))).is_some() {
            return None;
        }
        match select_list_slot(written, items) {
            Ok(found) => found.map(bind),
            Err(clash) => {
                *ambiguous.borrow_mut() = Some(clash);
                None
            }
        }
    });
    match ambiguous.into_inner() {
        Some(clash) => Err(clash),
        None => Ok(resolved),
    }
}

/// Resolves a whole ORDER BY clause, refusing what 8.3 refuses.
///
/// Names and positions for every item first and the arguments after, which is
/// the order 8.3 answers in: `ORDER BY 9, s LIKE 'a' ESCAPE '!!'` over one
/// select item is 1054 for the position there, and `ORDER BY nope LIKE 'a'
/// ESCAPE '!!'` is 1054 for the name rather than 1210.
///
/// `aggregated` says whether the query folds anything, because an aggregate in
/// the clause is only meaningful if it does.
fn resolve_order_by(
    order: Option<&OrderBy>,
    items: &[OutItem],
    refs: &[ColumnRef],
    aggregated: bool,
    bind: &dyn Fn(usize) -> Expr,
) -> EngineResult<Vec<SortKey>> {
    let Some(order) = order else {
        return Ok(Vec::new());
    };
    let OrderByKind::Expressions(written) = &order.kind else {
        return Err(unsupported("this ORDER BY form"));
    };

    // What `bind` may have put in an expression is in scope for the name check
    // below: the grouped caller binds a position to the hidden column carrying
    // the value already computed for it, which the relation has never heard of.
    let mut scope = refs.to_vec();
    scope.extend((0..items.len()).map(|index| ColumnRef::new("", alias_name(index))));

    let mut keys = Vec::with_capacity(written.len());
    for (position, item) in written.iter().enumerate() {
        let expr = resolve_order_expr(&item.expr, items, refs, bind)?;
        // Under this clause's own name rather than the field list's, which is
        // where the message used to come from: an ORDER BY name that resolved
        // against nothing was evaluated per row and reported wherever the
        // evaluation happened to fail.
        //
        // A name the *output* answers to is left alone rather than refused,
        // because it is resolved further downstream: `substitute_names` does
        // not descend into every expression, so `ORDER BY ABS(zz)` over an
        // alias `zz` reaches the sort unsubstituted, fails to evaluate against
        // a source row, and is retried against the projected result — which is
        // how this engine has always answered it.
        if let Some(name) = unresolved_column(&expr, &scope) {
            if !items.iter().any(|out| out.name.eq_ignore_ascii_case(&name)) {
                return Err(EngineError::unknown_column(&name, "order clause"));
            }
        }
        // After this item's names and before the next item is looked at all,
        // which is where 8.3 puts it: `ORDER BY MAX(nope)` is 1054, `ORDER BY
        // MAX(id), 9` is 3029, and `ORDER BY 9, MAX(id)` is 1054 for the
        // position.
        if !aggregated && contains_aggregate(&item.expr) {
            return Err(EngineError::aggregate_order_non_aggregated(position + 1));
        }
        keys.push(SortKey {
            expr,
            descending: item.options.asc == Some(false),
        });
    }
    refuse_at_resolve(written.iter().map(|i| &i.expr))?;
    Ok(keys)
}

/// Refuses an ORDER BY that DISTINCT has already thrown its sort key away for.
///
/// DISTINCT settles which rows there are before the sort reads them, so a key
/// that is not one of the columns the select list returns no longer exists per
/// row. 8.3 refuses with 3065 rather than picking one of the duplicates, and
/// this engine answered by sorting before the duplicates were removed.
///
/// What counts as being in the select list is the *column*, not the
/// expression: `SELECT DISTINCT id FROM t ORDER BY id + 1` answers on 8.3
/// because `id` is returned, and `SELECT DISTINCT id + 1 AS x FROM t ORDER BY
/// id` is 3065 because it is not — which is why this reads the item as it was
/// written. An item that resolved to a select-list position is in the list by
/// construction and is not read at all.
///
/// After every item has been resolved, not while they are: `SELECT DISTINCT s
/// FROM t ORDER BY id, nope` is 1054 for `nope` on 8.3, not 3065 for `id`.
///
/// A subquery is not descended into, so `ORDER BY (SELECT id)` is answered
/// where 8.3 raises 3065. That is the conservative direction `unresolved_column`
/// takes for the same reason, and the same gap.
fn check_distinct_order(
    order: Option<&OrderBy>,
    items: &[OutItem],
    refs: &[ColumnRef],
    relation: &Relation,
) -> EngineResult<()> {
    let Some(OrderByKind::Expressions(written)) = order.map(|o| &o.kind) else {
        return Ok(());
    };
    let returned: Vec<usize> = items.iter().filter_map(|item| item.slot).collect();
    for (position, item) in written.iter().enumerate() {
        // A position, or a bare name the select list answered, is in the list
        // by definition.
        if written_position(&item.expr).is_some() {
            continue;
        }
        if let Expr::Identifier(ident) = peeled(&item.expr) {
            if select_list_slot(&ident.value, items)?.is_some() {
                continue;
            }
        }
        // So is the whole expression written a second time, which is the one
        // way a select item that is not a column can be sorted on without
        // naming its alias: `SELECT DISTINCT id + 1 AS x FROM t ORDER BY id +
        // 1` answers on 8.3 where `ORDER BY id` is 3065.
        //
        // Compared as the parser rebuilds it and not as it was typed, because
        // 8.3 compares the expressions rather than the text: `ORDER BY id+1`
        // against `SELECT id + 1` answers there, and so does `ORDER BY
        // upper(s)` against `SELECT UPPER(s)`. Reading the source span instead
        // — which is what `render_expr` does, and what this first did — made
        // every one of those 3065.
        let rendered = item.expr.to_string();
        if items
            .iter()
            .any(|out| out.expr.to_string().eq_ignore_ascii_case(&rendered))
        {
            continue;
        }
        for slot in columns_read(&item.expr, refs) {
            if returned.contains(&slot) {
                continue;
            }
            // An enclosing query's column, which `refs` carries after this
            // query's own, has no place in this query's select list to be
            // compared against.
            let Some(column) = relation.columns.get(slot) else {
                continue;
            };
            // Only the parts there are: 8.3 names a stored table's column
            // `db.t.id` and a derived table's or a CTE's `d.id`, because a
            // derived table is in no schema. Writing the empty schema anyway
            // gave `.d.id`, a spelling no server produces and one this engine
            // disagreed with itself about — the same query over a CTE named
            // the schema, because a CTE's relation carries one.
            let written = [
                column.schema.as_str(),
                column.table.as_str(),
                column.name.as_str(),
            ]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(".");
            return Err(EngineError::order_not_in_select_list(
                position + 1,
                &written,
            ));
        }
    }
    Ok(())
}

/// Every relation column an expression reads directly.
///
/// Built on the same walk as `unresolved_column` and conservative in the same
/// direction: a variant missing from `subexpressions` is a column this does
/// not see, and the statement is answered as it is today.
fn columns_read(e: &Expr, refs: &[ColumnRef]) -> Vec<usize> {
    match e {
        Expr::Identifier(i) if i.value.starts_with('@') => Vec::new(),
        Expr::CompoundIdentifier(parts)
            if parts.first().is_some_and(|p| p.value.starts_with('@')) =>
        {
            Vec::new()
        }
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            column_slot(refs, e).into_iter().collect()
        }
        Expr::Function(f) => value_arguments(f)
            .iter()
            .flat_map(|a| columns_read(a, refs))
            .collect(),
        other => subexpressions(other)
            .into_iter()
            .flat_map(|s| columns_read(s, refs))
            .collect(),
    }
}

/// Refuses an ORDER BY over a finished result that names anything the result
/// does not carry.
///
/// For the two paths that sort what a query already returned: a set operation
/// and a SELECT with no FROM clause. Both resolve against the output columns
/// alone — `SELECT id AS q FROM t UNION SELECT b FROM t ORDER BY id` is 1054
/// on 8.3 even with `id` in plain sight in `t`, and so is `ORDER BY 2` over
/// one output column.
///
/// Hoisted out of any row loop deliberately: a position is only read while a
/// row is being keyed, so over an empty result an out-of-range one was never
/// looked at and the statement answered instead of being refused.
fn check_projected_order(
    columns: &[OutColumn],
    order: Option<&OrderBy>,
    outer: Option<&OuterRow>,
) -> EngineResult<()> {
    let Some(order) = order else { return Ok(()) };
    let OrderByKind::Expressions(written) = &order.kind else {
        return Err(unsupported("this ORDER BY form"));
    };
    // Without a database, for the reason `sort_rows` gives: what this sorts is
    // a result rather than a table reference.
    let mut refs: Vec<ColumnRef> = columns
        .iter()
        .map(|c| ColumnRef::new(&c.table, &c.name))
        .collect();
    // The enclosing query's columns too, or a correlated subquery would be
    // refused for naming one: `SELECT id, (SELECT 1 ORDER BY id) FROM t`
    // answers on 8.3, where `id` is the outer row's.
    if let Some(outer) = outer {
        refs.extend(outer.schema.iter().cloned());
    }
    for item in written {
        if let Some((position, digits)) = written_position(&item.expr) {
            if position == 0 || position > columns.len() {
                return Err(EngineError::unknown_column(digits, "order clause"));
            }
            continue;
        }
        if let Some(name) = unresolved_column(&item.expr, &refs) {
            return Err(EngineError::unknown_column(&name, "order clause"));
        }
    }
    // Out of the row loop for the reason `sort_rows` gives: `order_projected`
    // settles an ESCAPE per row, so over an empty union — or the one row a
    // FROM-less SELECT has, down a branch nothing takes — a malformed one was
    // never read and the statement answered where 8.3 raises 1210. The same
    // zero-rows shape as #0058 and #0093.
    refuse_at_resolve(written.iter().map(|i| &i.expr))?;
    Ok(())
}

/// Rewrites references to a select-list alias into the hidden column carrying
/// the value already computed for it, so HAVING and ORDER BY can name one.
///
/// A hidden column and not a rendered literal, for the reason `alias_name`
/// gives: the value goes through unchanged, tag and all.
fn substitute_aliases(e: &Expr, names: &[String]) -> Expr {
    use sqlparser::ast::Ident;

    substitute_names(e, &|written| {
        names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(written))
            .map(|position| Expr::Identifier(Ident::new(alias_name(position))))
    })
}

/// Rewrites a reference to a select-list item into the expression written for
/// it, for the path where nothing has been computed to bind instead.
///
/// One level and no further, which is what 8.3 does: `SELECT b + 1 AS b FROM t
/// HAVING b > 2` filters on `b + 1 > 2` there with the inner `b` the column,
/// and answers 3 and 4 rather than re-reading its own alias.
fn substitute_item_exprs(e: &Expr, projection: &[SelectItem]) -> Expr {
    substitute_names(e, &|written| {
        projection.iter().find_map(|item| match item {
            SelectItem::ExprWithAlias { expr, alias } => name::alias(alias)
                .eq_ignore_ascii_case(written)
                .then(|| expr.clone()),
            SelectItem::UnnamedExpr(inner) => render_expr(inner)
                .eq_ignore_ascii_case(written)
                .then(|| inner.clone()),
            // A `*` writes no name of its own: what it projects are columns,
            // which the row answers to directly.
            _ => None,
        })
    })
}

/// The walk both substitutions share.
///
/// One recursion rather than two, because a variant missing from it is a place
/// an alias silently fails to resolve — and the two callers would have had to
/// drift apart one variant at a time to find out.
fn substitute_names(e: &Expr, of: &dyn Fn(&str) -> Option<Expr>) -> Expr {
    if let Expr::Identifier(ident) = e {
        if let Some(replacement) = of(&ident.value) {
            return replacement;
        }
    }

    let recur = |inner: &Expr| Box::new(substitute_names(inner, of));
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

/// Functions whose answer is unsigned whatever they were given.
///
/// The counting windows number their rows from one, and a connection id and an
/// auto-increment cannot be negative. `ROW_COUNT()` is deliberately absent: it
/// answers -1 when the last statement had nothing to report.
const ALWAYS_UNSIGNED: &[&str] = &[
    "connection_id",
    "dense_rank",
    "last_insert_id",
    "ntile",
    "rank",
    "row_number",
    "year",
];

/// Functions whose answer is unsigned when their first argument is.
///
/// The second argument of `TRUNCATE(u, 0)` says how many places to keep and
/// says nothing about the sign, which is why this reads one argument where
/// [`unsigned_answer`]'s other families read all of them.
const UNSIGNED_FOLLOWS_FIRST: &[&str] = &["abs", "ceil", "ceiling", "floor", "round", "truncate"];

/// Whether a cast names a target that keeps the argument's certainty.
///
/// `CAST(1 AS UNSIGNED)` cannot fail and MySQL says NOT NULL; `CAST(1 AS CHAR)`
/// and `CAST(1 AS DATE)` are nullable there even for an argument that cannot
/// be, because a conversion to those can have no answer.
fn numeric_cast(data_type: &sqlparser::ast::DataType) -> bool {
    use sqlparser::ast::DataType as D;
    matches!(
        data_type,
        D::Signed | D::SignedInteger | D::Unsigned | D::UnsignedInteger
    ) || matches!(
        crate::types::SqlType::from_ast(data_type).map(|t| t.kind),
        Ok(crate::types::TypeKind::Int
            | crate::types::TypeKind::Decimal
            | crate::types::TypeKind::Float)
    )
}

/// Whether an expression is the literal NULL, which no conditional counts
/// against the sign of its answer.
///
/// Measured on 8.3: `COALESCE(NULL, iu)`, `IFNULL(NULL, iu)` and `IF(1, iu,
/// NULL)` all report UNSIGNED, as does a `CASE` with no `ELSE` at all. A
/// *column* holding NULL is a different thing and is read for its declaration,
/// which is why this looks at the literal and not at a value.
fn is_null_literal(e: &Expr) -> bool {
    match e {
        Expr::Nested(inner) => is_null_literal(inner),
        Expr::Value(v) => matches!(v.value, sqlparser::ast::Value::Null),
        _ => false,
    }
}

/// Whether a computed column's values are bytes rather than characters.
///
/// Two functions, and both were measured: 8.3 describes `UNHEX(x)` and
/// `FROM_BASE64(x)` with the binary character set and the BINARY flag, and
/// their partners `HEX(x)` and `TO_BASE64(x)` with the connection's character
/// set and no flag at all. The pair is the point — one direction of each
/// conversion produces bytes and the other produces the characters that
/// describe them.
///
/// Not MySQL's rule, which is the expression's collation and travels through
/// every string function that touches a binary operand. That is #0128, and
/// #0143 for the aggregate that loses it. This answers only for the function
/// at the root, so `UNHEX('4142')` is bytes and `CONCAT(UNHEX('4142'), '')` is
/// not — which is the same shape MySQL gets right and this engine does not,
/// rather than a new one.
///
/// It is not the whole of what carries binariness either: a derived table, a
/// CTE and a view never reach this, because they rebuild their columns from an
/// `SqlType`. `OutColumn::sql_type` is the other half.
fn binary_answer(e: &Expr, refs: &[ColumnRef], sub: Option<Sub<'_>>) -> bool {
    match e {
        Expr::Nested(inner) => binary_answer(inner, refs, sub),
        // Unary plus is not an operator — the parser hands its operand back —
        // so it answers whatever its operand does, exactly as parentheses do.
        // `+X'41'` is binary on 8.3 where `-X'41'` is a DOUBLE and not a
        // string at all.
        Expr::UnaryOp {
            op: sqlparser::ast::UnaryOperator::Plus,
            expr,
        } => binary_answer(expr, refs, sub),
        // A hex literal *is* bytes. `X'41'` is the binary character set on
        // 8.3, and the engine held it as `Value::Bytes` all along while
        // describing the column in the connection's character set — so a
        // client that reads this bit to decide between a Buffer and a String
        // was told the wrong one. `UNHEX('41')` beside it, which produces the
        // same byte, was already right.
        Expr::Value(v) => matches!(v.value, sqlparser::ast::Value::HexStringLiteral(_)),
        Expr::Function(f) => matches!(
            f.name.to_string().to_ascii_lowercase().as_str(),
            "unhex" | "from_base64"
        ),
        // A scalar subquery hands back its column's values, bytes and all:
        // 8.3 describes `(SELECT bl FROM T)` over a `BLOB` and
        // `(SELECT bn FROM T)` over a `BINARY(4)` with the binary character
        // set, and their `TEXT` and `VARCHAR` neighbours with the
        // connection's.
        //
        // Asked only of the codes where it is a *choice*. A number, a temporal
        // and a document already have their answer from the type — and saying
        // "bytes" of one of those would go further than the wire: a relation
        // built from this column reads `OutColumn::sql_type`, which turns a
        // binary computed column into `varbinary`, so a subquery over a `DATE`
        // inside a derived table would come back as bytes rather than a date.
        Expr::Subquery(query) => {
            let textual = |t: ColumnType| {
                matches!(
                    t,
                    ColumnType::VarString
                        | ColumnType::String
                        | ColumnType::Blob
                        | ColumnType::TinyBlob
                        | ColumnType::MediumBlob
                        | ColumnType::LongBlob
                )
            };
            subquery_column(sub, query, refs).is_some_and(|column| {
                textual(column.ty)
                    && column.to_wire().charset
                        == mysql_ultralight_protocol::column::charset::BINARY
            })
        }
        _ => false,
    }
}

/// Whether an expression's values are unsigned on the wire.
///
/// Measured against MySQL 8.3, and the families do not agree with each other:
/// arithmetic keeps the flag when *either* side has it, so `u + 1` and `u - 1`
/// are both unsigned, where `COALESCE(u, 1)` is signed because a conditional
/// needs *all* of its candidates to be. `%` reads only its left operand. `-u`
/// drops it, and so does anything that is no longer an integer — `u + 1.5` and
/// `u / 2` are both signed.
///
/// This has to agree with what the *value* carries, or the two protocols
/// disagree with each other: a column announced UNSIGNED over a negative
/// number sends `-93` as text and the same bytes to a binary client reading
/// 18446744073709551523. #0069 made every arithmetic answer that would go
/// negative a refusal instead, which is what let subtraction back into the
/// list here; the untagging in [`crate::value::Value::untag_unsigned`] is the
/// other half of holding the two together.
///
/// Defaults to signed, which is still the safe direction: an expression whose
/// sign nothing here recognises answers within the signed range or not at all.
fn unsigned_answer(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
) -> bool {
    let recur = |inner: &Expr| unsigned_answer(inner, refs, input, sub);
    let integral = |e: &Expr| is_integer(computed_column_type(e, refs, input, sub));
    // The whole of the rule for the families that build a *new* item out of
    // several candidates — the conditional functions, `CASE`, and the window
    // functions. Those aggregate a type across the candidates and take the
    // sign from the aggregate rather than from any one of them, so the flag
    // survives only where that aggregate is an integer.
    //
    // Every operand that was ever unsigned here answers the same to both
    // questions — a column, a twenty-digit literal, a cast, a bitwise answer
    // are all integers — and a hex literal is what tells them apart: `X'41'`
    // is UNSIGNED on 8.3 while `COALESCE(X'41', X'42')`, `IFNULL(X'41', '')`,
    // `GREATEST(X'41', X'42')`, `IF(1, X'41', X'42')`,
    // `CASE WHEN 1 THEN X'41' END` and `LAG(X'41') OVER ()` are all signed. So
    // is `GREATEST(iu, 'a')`, where an unsigned column aggregates with a
    // string to a VAR_STRING.
    //
    // Not asked of `MIN`, `MAX`, `ABS` or `NULLIF`, and that is measured
    // rather than an omission: those hand one item back instead of aggregating
    // over several, so `MIN(X'41')` is a VAR_STRING that is UNSIGNED and
    // `ABS(X'41')` a DOUBLE that is.
    //
    // A closure, not a value: `integral` reaches `computed_column_type`, which
    // runs a probe query for a scalar subquery, and most of the arms below
    // never need the answer.
    let aggregates_to_integer = || integral(e);
    // The corrected shape, as in the arms below that ask `reassociated` for
    // it. `a XOR b IS NULL` arrives with the `IS NULL` at its root and means
    // `a XOR (b IS NULL)`, which is a different expression to read the sign
    // of.
    //
    // It changes no answer today and cannot be made to: the only operators
    // these corrections lift to the root are the connectives and the
    // comparisons, and none of those is UNSIGNED. MySQL's grammar is what
    // keeps a bitwise operator — the one family that *is* UNSIGNED — out of
    // the position: a `bit_expr` cannot hold a `predicate`, so
    // `SELECT 2 IN (2) | 4` and `SELECT 'a' LIKE 'a' + 1` are both 1064 on
    // 8.3. That is a fact about MySQL's grammar and this engine's level
    // table, not something this function states, and the three walkers over
    // these trees have to read the same shape as `eval` does.
    if let Some(fixed) = crate::expr::reassociated_postfix(e) {
        return recur(&fixed);
    }
    match e {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            direct_column(e, refs, input).is_some_and(|column| column.ty.unsigned)
        }
        Expr::Nested(inner) => recur(inner),
        // A scalar subquery keeps its column's sign, as it keeps its type:
        // `(SELECT su FROM T) + 1` over an `INT UNSIGNED` is UNSIGNED on 8.3,
        // and so is `(SELECT yr FROM T) + 1` over a `YEAR`. Read off the wire
        // definition rather than off `OutColumn::unsigned`, because a YEAR
        // carries the flag from its type code and not from a field.
        Expr::Subquery(query) => subquery_column(sub, query, refs).is_some_and(|column| {
            column.to_wire().flags & mysql_ultralight_protocol::column::flags::UNSIGNED != 0
        }),
        // A whole number too big for an `i64` is a BIGINT UNSIGNED in MySQL,
        // which is the one literal that carries the flag: `SELECT
        // 18446744073709551615 - 1` is UNSIGNED there and answers
        // 18446744073709551614, where a signed column would have handed a
        // binary client -2.
        Expr::Value(v) => match &v.value {
            sqlparser::ast::Value::Number(text, _) => {
                text.parse::<i64>().is_err() && text.parse::<u64>().is_ok()
            }
            // A hex literal carries the flag whatever it holds, which is the
            // one place UNSIGNED describes something that is not a number:
            // `X'41'`, `x'41'` and `0x41` are all VAR_STRING with the binary
            // character set and UNSIGNED on 8.3, and so are `X''` and a
            // thirty-two byte one. A *bit* literal is not — `b'101'`, `B'101'`
            // and `0b101` are the same VAR_STRING with the flag missing —
            // which is why this names one spelling and not the family. (Bit
            // literals do not parse here at all; that half is #0141.)
            //
            // Sending it matters for the same reason it matters on a number.
            // A client reads the definition before the row, and Prisma reads
            // this bit to decide how to decode; a column MySQL calls UNSIGNED
            // and this engine calls signed is a difference the caller can see
            // without ever looking at the bytes.
            sqlparser::ast::Value::HexStringLiteral(_) => true,
            _ => false,
        },
        // A cast names the signedness in the statement itself.
        Expr::Cast { data_type, .. } => {
            matches!(data_type, sqlparser::ast::DataType::Unsigned)
                || matches!(data_type, sqlparser::ast::DataType::UnsignedInteger)
        }
        Expr::BinaryOp { left, op, right } => {
            use sqlparser::ast::BinaryOperator as B;
            // The corrected shape, for the same reason `computed_column_type`
            // uses it: `2 ^ 3 / 2` arrives with `^` at its root and is really
            // a division, which MySQL reports signed. Reading the root as
            // parsed would have flagged a decimal column UNSIGNED.
            if let Some(fixed) = crate::expr::reassociated(op, left, right) {
                return recur(&fixed);
            }
            // Every bitwise operator answers a BIGINT UNSIGNED whatever it was
            // given, which is the one family here that does not have to argue
            // about its operands: `-1 & 1` is 1 and `-1 >> 1` is
            // 9223372036854775807. Safe for `never_negative` to read too,
            // because an answer this engine cannot hold as a non-negative
            // `i64` is refused rather than returned — see `expr::bit_pattern`.
            if matches!(
                op,
                B::BitwiseAnd
                    | B::BitwiseOr
                    | B::BitwiseXor
                    | B::PGBitwiseShiftLeft
                    | B::PGBitwiseShiftRight
            ) {
                return true;
            }
            // `MOD` reads only its left operand, because that is the one its
            // answer takes its sign from: measured on 8.3, `iu MOD -1` is
            // UNSIGNED and `-1 MOD iu` is not. It keeps the flag whatever the
            // result type is, where the three below need an integer one —
            // `iu MOD 1e0` is a DOUBLE and still UNSIGNED, while `iu + 'x'` is
            // a DOUBLE and signed.
            if matches!(op, B::Modulo) {
                return recur(left);
            }
            // Subtraction is back in the list. It was left out while the
            // engine held an `i64` and answered `u - 100` with -93, which a
            // UNSIGNED flag would have turned into 18446744073709551523 for a
            // binary client; now that a subtraction leaving the range is
            // refused with 1690, as MySQL refuses it, the flag is true again.
            // See #0069.
            //
            // `NO_UNSIGNED_SUBTRACTION` takes it back out for the length of
            // the statement, and takes nothing else out: under it `iu - 100`
            // is signed and `iu + -100` still raises 1690.
            if matches!(op, B::Minus) && no_unsigned_subtraction() {
                return false;
            }
            matches!(op, B::Plus | B::Minus | B::Multiply | B::MyIntegerDivide)
                && integral(e)
                && (recur(left) || recur(right))
        }
        // `~x` is a BIGINT UNSIGNED like the binary bitwise operators. A sign
        // is not — MySQL reports `-(7 ^ 3)` signed — which is what the
        // fall-through leaves.
        //
        // Unary plus is not a sign: it is discarded by the parser, so the
        // operand answers for it. `NULLIF(+u, 1)` over an `INT UNSIGNED` is
        // UNSIGNED on 8.3 where `NULLIF(-u, 1)` is signed, and a flag dropped
        // here reaches a binary client as a negative number.
        Expr::UnaryOp { op, expr } => match crate::expr::reassociated_unary(op, expr) {
            Some(fixed) => recur(&fixed),
            None => match op {
                sqlparser::ast::UnaryOperator::BitwiseNot => true,
                sqlparser::ast::UnaryOperator::Plus => recur(expr),
                _ => false,
            },
        },
        // `CEIL` and `FLOOR` arrive as their own nodes rather than as calls.
        Expr::Ceil { expr, .. } | Expr::Floor { expr, .. } => recur(expr),
        // Every candidate has to be unsigned; a signed `ELSE` gives it away.
        // A *missing* one does not, and neither does a NULL: measured on 8.3,
        // `CASE WHEN 1 THEN iu END` and `IF(1, iu, NULL)` are both UNSIGNED,
        // where reading the absent branch as a signed candidate made the first
        // of them signed.
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            // Both halves read *every* candidate, the `ELSE` included. Reading
            // only the `WHEN` results here described `CASE WHEN 0 THEN NULL
            // ELSE bu END` signed while the value kept its unsigned tag — the
            // text protocol writing every digit of 18446744073709551615 and a
            // binary client reading the same eight bytes as -1. `COALESCE(NULL,
            // bu)` and `IF(0, NULL, bu)` are UNSIGNED on 8.3 and this is the
            // same expression written a third way.
            let candidates = || {
                conditions
                    .iter()
                    .map(|w| &w.result)
                    .chain(else_result.as_deref())
            };
            aggregates_to_integer()
                && candidates().any(&recur)
                && candidates().all(|r| is_null_literal(r) || recur(r))
        }
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_lowercase();
            if ALWAYS_UNSIGNED.contains(&name.as_str()) {
                return true;
            }
            let args = function_arguments(f);
            if args.is_empty() {
                return false;
            }
            match name.as_str() {
                // These hand back one of their arguments unchanged, so the
                // flag survives only if every candidate carries it: MySQL
                // makes `COALESCE(u, 1)` signed on the strength of the `1`.
                //
                // And `aggregates_to_integer` on top of that, which is where
                // the rest of the reasoning lives.
                "coalesce" | "ifnull" | "nvl" | "greatest" | "least" => {
                    aggregates_to_integer()
                        && args.iter().any(&recur)
                        && args.iter().all(|a| is_null_literal(a) || recur(a))
                }
                "if" => {
                    aggregates_to_integer()
                        && args.len() == 3
                        && args[1..].iter().any(&recur)
                        && args[1..].iter().all(|a| is_null_literal(a) || recur(a))
                }
                // MIN and MAX answer one of the rows they folded; SUM and AVG
                // widen past the column and come back signed.
                // `aggregates_to_integer` is deliberately not asked here; the
                // reason is recorded on it.
                "min" | "max" => args.iter().all(&recur),
                // A window reaching past the frame answers its default, which
                // is why `LAG(u, 1, 0)` is signed — the `0` is. The *offset*
                // is not a candidate and does not count, which is measured:
                // `LAG(u, 1)` and `NTH_VALUE(u, 1)` are both UNSIGNED on 8.3
                // though the 1 in each is not. The value and the default
                // meet in one type, which is what `aggregates_to_integer`
                // reads.
                "lag" | "lead" => {
                    aggregates_to_integer()
                        && args.first().is_some_and(&recur)
                        && args.get(2).is_none_or(|d| is_null_literal(d) || recur(d))
                }
                // `NTH_VALUE` is described here and by `computed_column_type`
                // and evaluated by neither: the window dispatch handles the
                // other four and this one falls through to the aggregate arm,
                // which does not know it. Described rather than removed
                // because that is how it arrived; it needs implementing, not
                // hiding.
                "first_value" | "last_value" | "nth_value" => {
                    aggregates_to_integer() && args.first().is_some_and(&recur)
                }
                // `MOD(a, b)` is `a % b` spelled as a call and takes its
                // sign the same way, from its dividend alone.
                "mod" => args.first().is_some_and(&recur),
                // `NULLIF(a, b)` answers `a` or NULL, and
                // `computed_column_type` already types it by that argument
                // alone. The sign goes the same way and did not, which the hex
                // literal is what turned up: `NULLIF(X'41', 'z')` is UNSIGNED
                // on 8.3, and so is `NULLIF(iu, 1)` over an `INT UNSIGNED` and
                // `NULLIF(bu, 1)` over a `BIGINT UNSIGNED` — the second of
                // which this engine described signed while sending every digit
                // of an unsigned value, so a binary client reading it over
                // 18446744073709551615 got -1. Not asked
                // `aggregates_to_integer` either, because there is nothing to
                // aggregate: the second argument is compared against, never
                // returned.
                "nullif" => args.first().is_some_and(&recur),
                _ => {
                    UNSIGNED_FOLLOWS_FIRST.contains(&name.as_str())
                        && args.first().is_some_and(&recur)
                }
            }
        }
        _ => false,
    }
}

/// Functions whose answer cannot be NULL when their arguments cannot.
///
/// Measured by calling each with a constant, and the split is not the obvious
/// one: `LENGTH('a')` is NOT NULL where `UPPER('a')` is not, and `ABS(1)` is
/// where `SQRT(4)` is not. A function that can fail on a bad domain or a zero
/// divisor stays nullable however it was called, which is why `MOD` is absent
/// and `%` is not an operator here either.
const ARGUMENT_FOLLOWING: &[&str] = &[
    "abs",
    "ascii",
    "ceil",
    "ceiling",
    "char_length",
    "character_length",
    "field",
    "floor",
    "greatest",
    "instr",
    "least",
    "length",
    "locate",
    "octet_length",
    "ord",
    // NULL for a NULL argument — `REGEXP_LIKE(NULL, 'a')` is NULL on 8.3 —
    // and NOT NULL otherwise, which is the whole of this list. Its sibling
    // predicates are not here: `JSON_VALID('1')` is described *nullable*
    // there even though it cannot be, and agreeing with MySQL beats being
    // consistent with it.
    "regexp_like",
    "round",
    "sign",
    "truncate",
];

/// Functions whose answer is never NULL whatever they were given.
///
/// `COUNT` counts, and counts nothing as zero. The ranking window functions
/// number their rows from one. Every other aggregate — `SUM`, `MIN`, `AVG`,
/// even `SUM(1)` — is nullable, because an empty group has no answer.
const ALWAYS_NOT_NULL: &[&str] = &["count", "dense_rank", "isnull", "rank", "row_number"];

/// Functions that answer with the moment or the connection, and only when
/// asked for nothing.
///
/// `UNIX_TIMESTAMP()` is NOT NULL where `UNIX_TIMESTAMP(NULL)` is NULL, so the
/// argument count is the rule rather than the name. `DATABASE()`, `USER()` and
/// `UUID()` are measured nullable and are deliberately not here.
const NULLARY_NOT_NULL: &[&str] = &[
    "connection_id",
    "last_insert_id",
    "pi",
    "rand",
    "row_count",
    "unix_timestamp",
    "version",
];

/// The clock functions, whose one argument is a *precision* and not a value.
///
/// Split out of [`NULLARY_NOT_NULL`] because the argument-count rule was
/// measured against `UNIX_TIMESTAMP` and does not hold here: `NOW(3)` and
/// `CURTIME(3)` are NOT NULL on 8.3, and `NOW(NULL)` is a syntax error rather
/// than a NULL — there is no value to pass in that could make one.
const CLOCK_NOT_NULL: &[&str] = &[
    "curdate",
    "current_date",
    "current_time",
    "current_timestamp",
    "curtime",
    "localtime",
    "localtimestamp",
    "now",
    "sysdate",
];

/// Whether an expression can be said to be NOT NULL on the wire.
///
/// Under-reporting is safe and over-reporting is not: a column described
/// nullable can still carry values, where a NOT NULL flag is a promise a client
/// acts on — Prisma maps it to a non-optional field. So every rule here was
/// measured against MySQL 8.3, and anything unmeasured falls through to
/// nullable rather than being reasoned about.
fn never_null(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
) -> bool {
    let recur = |inner: &Expr| never_null(inner, refs, input, sub);
    // The corrected shape, as in the `BinaryOp` arm below: `a XOR b IS NULL`
    // arrives with the `IS NULL` at its root, where MySQL means
    // `a XOR (b IS NULL)` — nullable whenever `a` is, and the arm below that
    // answers NOT NULL for anything ending in `IS NULL` would have promised a
    // client a column that can hold NULL cannot.
    if let Some(fixed) = crate::expr::reassociated_postfix(e) {
        return recur(&fixed);
    }
    match e {
        Expr::Value(v) => !matches!(v.value, sqlparser::ast::Value::Null),
        // A column carries its own answer, which is the whole reason this
        // takes the relation: `id + 1` over a NOT NULL column is NOT NULL, and
        // reading the identifier as unknown made every arithmetic column
        // nullable.
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            direct_column(e, refs, input).is_some_and(|column| !column.nullable)
        }
        Expr::Nested(inner) => recur(inner),
        // A scalar subquery is NOT NULL only where its column is *and* the
        // query cannot come back empty, which is why this asks two questions
        // where every other arm asks one. 8.3 takes the flag off
        // `(SELECT sid FROM T)` over a column declared NOT NULL — the table
        // can be empty, and an empty scalar subquery is NULL — and keeps it on
        // `(SELECT 1)`, which reads no table.
        Expr::Subquery(query) => {
            always_one_row(query)
                && subquery_column(sub, query, refs).is_some_and(|column| !column.nullable)
        }
        // Only to a *numeric* target. `CAST(1 AS UNSIGNED)` is NOT NULL and
        // `CAST(1 AS CHAR)` is not, and nor is a cast to DATE — MySQL keeps
        // the door open for a conversion that cannot be made.
        Expr::Cast {
            data_type, expr, ..
        } => numeric_cast(data_type) && recur(expr),
        // `NOT`, a sign and `~` propagate.
        Expr::UnaryOp { op, expr } => {
            use sqlparser::ast::UnaryOperator as U;
            if let Some(fixed) = crate::expr::reassociated_unary(op, expr) {
                return recur(&fixed);
            }
            matches!(op, U::Minus | U::Plus | U::Not | U::BitwiseNot) && recur(expr)
        }
        // Asking whether something is NULL always has an answer, whatever the
        // something is.
        Expr::IsNull(_)
        | Expr::IsNotNull(_)
        | Expr::IsTrue(_)
        | Expr::IsFalse(_)
        | Expr::IsNotTrue(_)
        | Expr::IsNotFalse(_)
        | Expr::Exists { .. } => true,
        Expr::BinaryOp { left, op, right } => {
            use sqlparser::ast::BinaryOperator as B;
            // The corrected shape, as in `unsigned_answer`: `2 ^ 3 / 2` has
            // `^` at its root as parsed and is really a division, which is
            // nullable because the divisor can be zero.
            if let Some(fixed) = crate::expr::reassociated(op, left, right) {
                return recur(&fixed);
            }
            match op {
                // A bitwise operator has no zero-divisor case to answer NULL
                // for: every bit pattern is an answer, so it is NOT NULL
                // whenever its operands are.
                B::BitwiseAnd
                | B::BitwiseOr
                | B::BitwiseXor
                | B::PGBitwiseShiftLeft
                | B::PGBitwiseShiftRight => recur(left) && recur(right),
                // Division and the remainder answer NULL for a zero divisor,
                // so they are nullable however sound their operands are.
                B::Plus
                | B::Minus
                | B::Multiply
                | B::Eq
                | B::NotEq
                | B::Lt
                | B::LtEq
                | B::Gt
                | B::GtEq
                | B::And
                | B::Or
                | B::Xor => recur(left) && recur(right),
                // `||` meaning OR, which is what it means unless
                // PIPES_AS_CONCAT says otherwise. Only that half: MySQL calls
                // a concatenation of two literals NOT NULL where this engine
                // calls it nullable, which is a gap of its own and not this
                // one.
                B::StringConcat if !pipes_as_concat() => recur(left) && recur(right),
                // `<=>` is the one comparison that answers whatever it is
                // given, NULL included, so it is NOT NULL however nullable its
                // operands are — which is why it is here and not in the list
                // above.
                B::Spaceship => true,
                _ => false,
            }
        }
        Expr::InList { expr, list, .. } => recur(expr) && list.iter().all(recur),
        Expr::Between {
            expr, low, high, ..
        } => recur(expr) && recur(low) && recur(high),
        // The ESCAPE clause counts as an operand for the *flag* and not for
        // the answer: `SELECT 'a%' LIKE 'a\%' ESCAPE NULL` is 1 on 8.3 and
        // never NULL — a NULL escape leaves the default backslash in place —
        // yet the column comes back without NOT_NULL. Measured, because
        // reasoning from the values says the opposite.
        Expr::Like {
            expr,
            pattern,
            escape_char,
            ..
        }
        | Expr::ILike {
            expr,
            pattern,
            escape_char,
            ..
        } => {
            let escape_known = !matches!(
                escape_char.as_ref().map(|c| &c.value),
                Some(sqlparser::ast::Value::Null)
            );
            recur(expr) && recur(pattern) && escape_known
        }
        Expr::RLike { expr, pattern, .. } => recur(expr) && recur(pattern),
        // Only the results decide, and only with an `ELSE`: without one a
        // statement that matches no branch answers NULL.
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            else_result.as_deref().is_some_and(&recur)
                && conditions.iter().all(|w| recur(&w.result))
        }
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_lowercase();
            let args = function_arguments(f);
            if ALWAYS_NOT_NULL.contains(&name.as_str()) || CLOCK_NOT_NULL.contains(&name.as_str()) {
                return true;
            }
            if args.is_empty() {
                return NULLARY_NOT_NULL.contains(&name.as_str());
            }
            match name.as_str() {
                // These hand back whichever argument was not NULL, so one that
                // cannot be is enough.
                "coalesce" | "ifnull" | "nvl" => args.iter().any(&recur),
                // `IF` picks between the second and the third; the condition
                // decides nothing, so `IF(v, 1, 2)` is NOT NULL for a nullable
                // `v`. Only at that arity — `all` over the empty slice `IF(v)`
                // leaves is true, which would promise NOT NULL for a call that
                // answers NULL.
                "if" => args.len() == 3 && args[1..].iter().all(&recur),
                // A window function reaching past the frame answers its
                // default, and has none unless one was given.
                "lag" | "lead" => args.len() >= 3 && args.iter().all(&recur),
                _ => ARGUMENT_FOLLOWING.contains(&name.as_str()) && args.iter().all(&recur),
            }
        }
        // `CEIL` and `FLOOR` arrive as their own nodes rather than as calls,
        // and follow their argument like the `CEILING` spelling of the same
        // function.
        Expr::Ceil { expr, .. } | Expr::Floor { expr, .. } => recur(expr),
        _ => false,
    }
}

/// The MySQL type code for a function's result, where it does not depend on
/// the arguments.
///
/// Every entry measured against MySQL 8.3 rather than reasoned about. The
/// asymmetries are the reason: `LENGTH` is a `LONGLONG` while `CONCAT` is a
/// `VAR_STRING`, `JSON_TYPE` is a string while `JSON_LENGTH` is a number, and
/// `JSON_UNQUOTE` is a `LONG_BLOB` where `JSON_QUOTE` is a `VAR_STRING`.
///
/// Only names that reach here. `FLOOR`, `CEIL`, `POSITION`, `SUBSTRING`,
/// `SUBSTR` and `TRIM` have their own grammar in `sqlparser`, so they arrive
/// as `Expr::Floor` and friends and never as a call — listing them here would
/// make the table look like it covers cases it does not. Their near-synonyms
/// `CEILING`, `MID` and `LTRIM` do parse as calls and so are here.
fn function_result_type(name: &str) -> Option<ColumnType> {
    Some(match name {
        // Counting and measuring, which are always integers.
        "length" | "char_length" | "character_length" | "octet_length" | "instr" | "locate"
        | "field" | "count" | "row_number" | "rank" | "dense_rank" | "connection_id"
        | "last_insert_id" | "row_count" | "unix_timestamp" => ColumnType::LongLong,
        // The date parts, which are numbers rather than dates.
        "timestampdiff" | "datediff" | "hour" | "minute" | "second" | "microsecond" | "quarter"
        | "dayofweek" | "dayofyear" | "dayofmonth" | "day" | "month" | "weekday" => {
            ColumnType::LongLong
        }
        // `EXTRACT` with one of the compound units, which `crate::rewrite`
        // turns into a call because sqlparser has no unit for it. MySQL sends
        // a LONGLONG for every unit, compound ones included — measured — which
        // is what the `Expr::Extract` arm below says for the other nine.
        "ultralight_extract" => ColumnType::LongLong,
        // The one arithmetic function whose answer does not follow its
        // argument's kind: `SIGN(1.50)`, `SIGN(1.5e0)` and `SIGN(iu)` are all
        // a signed LONGLONG of length 21 on 8.3, where `ABS` of the same three
        // is a NEWDECIMAL, a DOUBLE and an *unsigned* LONGLONG. So it is typed
        // here rather than through `numeric_answer`, and it is deliberately
        // absent from `UNSIGNED_FOLLOWS_FIRST`.
        "sign" => ColumnType::LongLong,
        "year" => ColumnType::Year,
        "curdate" | "current_date" | "last_day" | "date" => ColumnType::Date,
        "now" | "current_timestamp" | "localtime" | "localtimestamp" | "sysdate" => {
            ColumnType::DateTime
        }
        "curtime" | "current_time" => ColumnType::Time,
        // Documents. `JSON_UNQUOTE` is text rather than a document, and MySQL
        // sends it as a long blob.
        "json_object" | "json_array" | "json_extract" | "json_set" | "json_array_append"
        | "json_merge_patch" | "json_keys" | "json_arrayagg" => ColumnType::Json,
        // One gap left here deliberately: 8.3 describes `GROUP_CONCAT` as a
        // `VAR_STRING` rather than a `LONG_BLOB` when `group_concat_max_len`
        // is 512 or less — measured, 512 is a `VAR_STRING` and 513 is a
        // `LONG_BLOB` — and this walk has no session to ask. Threading one
        // through its twenty-odd call sites is a change about the type, not
        // about #0088, and until somebody lowers the variable the two agree.
        "json_unquote" | "group_concat" => ColumnType::LongBlob,
        // Every predicate answers an integer. `regexp_like` had been left out
        // and was described VAR_STRING while answering 0 or 1 — the same
        // defect `<=>` had, found the same way. `isnull` is typed with the
        // argument-dependent group above rather than here.
        "json_valid" | "json_length" | "json_contains" | "regexp_like" => ColumnType::LongLong,
        // Everything that hands back a string.
        "concat" | "concat_ws" | "upper" | "ucase" | "lower" | "lcase" | "ltrim" | "rtrim"
        | "mid" | "left" | "right" | "replace" | "reverse" | "space" | "lpad" | "rpad" | "md5"
        | "date_format" | "json_type" | "json_quote" | "database" | "schema" | "version"
        | "user" | "current_user" | "session_user" | "system_user" | "uuid" => {
            ColumnType::VarString
        }
        // The byte-and-character conversions. `BIN` is a VAR_STRING whatever
        // it was given, because its answer is at most sixty-four digits.
        //
        // The other four are a VAR_STRING only while the answer fits one, and
        // this arm is never reached for them: `widens_with_argument` names all
        // four, so `computed_column_type` sizes them from their argument
        // before it asks here. They stay listed so the table covers the family
        // rather than half of it, and `VAR_STRING` is what the widening falls
        // back to for a width it cannot model. #0142.
        "hex" | "unhex" | "to_base64" | "from_base64" | "bin" => ColumnType::VarString,
        _ => return None,
    })
}

/// The type of a function that answers a number whatever it was given.
///
/// MySQL keeps the argument's kind and widens integers: `ABS(tinyint)` is a
/// LONGLONG, `ABS(decimal)` a NEWDECIMAL, and anything with no numeric kind of
/// its own — a string, a NULL — a DOUBLE. `whole` says the answer has no
/// fraction, which turns an exact argument into an integer: `CEIL(decimal)` is
/// a LONGLONG where `ABS(decimal)` is not.
fn numeric_answer(argument: ColumnType, whole: bool) -> ColumnType {
    match argument {
        t if is_integer(t) => ColumnType::LongLong,
        ColumnType::Decimal | ColumnType::NewDecimal if whole => ColumnType::LongLong,
        // Not FLOAT, which does not survive being computed with: MySQL types
        // `ABS(f)` and `ROUND(f, 2)` DOUBLE and shows every digit of the
        // widening, the same way `f + 0` does.
        ColumnType::Float => ColumnType::Double,
        t @ (ColumnType::Decimal | ColumnType::NewDecimal | ColumnType::Double) => t,
        _ => ColumnType::Double,
    }
}

/// Whether a code is one of the temporal types.
fn is_temporal(t: ColumnType) -> bool {
    matches!(
        t,
        ColumnType::Date | ColumnType::Time | ColumnType::DateTime | ColumnType::Timestamp
    )
}

/// The type an operand contributes to arithmetic over it.
///
/// A temporal is not a type MySQL computes *in*: it converts to its packed
/// number and the arithmetic happens over that, so the operand behaves as an
/// integer — or as a decimal when it carries fractional seconds, which pack
/// into the digits after the point. Measured on 8.3: `d + 0` is a LONGLONG
/// and `dt3 + 0` a NEWDECIMAL.
fn arithmetic_operand_type(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
) -> ColumnType {
    let ty = computed_column_type(e, refs, input, sub);
    if !is_temporal(ty) {
        return ty;
    }
    if temporal_precision(e, refs, input, sub) > 0 {
        ColumnType::NewDecimal
    } else {
        ColumnType::LongLong
    }
}

/// The fractional-second precision a temporal expression carries.
///
/// Only where it is written down — a column's declaration, a cast's target,
/// and the argument `NOW(3)` takes. Everything else answers zero, which is
/// what a temporal with no declared precision has; guessing higher would type
/// `CURDATE() + 0` a decimal where MySQL says LONGLONG.
fn temporal_precision(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
) -> u64 {
    use sqlparser::ast::DataType as D;
    match e {
        Expr::Nested(inner) => temporal_precision(inner, refs, input, sub),
        // A scalar subquery keeps the places its column declared, which is
        // what decides whether arithmetic over it is exact: 8.3 answers
        // NEWDECIMAL(3) for `(SELECT dt FROM T) + 0` over a `DATETIME(3)` and
        // LONGLONG for the same over a `DATE`, exactly as it does for the
        // column read directly.
        //
        // A *declared* precision only. A computed temporal has none to carry —
        // `OutColumn` has no field for one — so `(SELECT NOW(3)) + 0` is a
        // LONGLONG here where 8.3 says NEWDECIMAL(3). That is not this
        // subquery's doing: `SELECT x + 0 FROM (SELECT NOW(3) AS x) d` loses
        // it the same way, because every relation this engine builds from a
        // computed column rebuilds it from a type code with no scale in it.
        Expr::Subquery(query) => subquery_column(sub, query, refs)
            .and_then(|column| column.declared.and_then(|declared| declared.length))
            .unwrap_or(0),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => direct_column(e, refs, input)
            .and_then(|column| column.ty.length)
            .unwrap_or(0),
        Expr::Cast { data_type, .. } => match data_type {
            D::Datetime(precision) | D::Time(precision, _) => precision.unwrap_or(0),
            D::Timestamp(precision, _) => precision.unwrap_or(0),
            _ => 0,
        },
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_lowercase();
            let args = function_arguments(f);
            let widest = |candidates: &[Expr]| {
                candidates
                    .iter()
                    .map(|a| temporal_precision(a, refs, input, sub))
                    .max()
                    .unwrap_or(0)
            };
            let first = |n: usize| widest(args.get(..n).unwrap_or(&args));
            match name.as_str() {
                // The clock functions carry their precision as an argument.
                // The literal digit count, read off the token: the value is
                // evaluated elsewhere and this runs before any row exists.
                "now" | "current_timestamp" | "localtime" | "localtimestamp" | "sysdate"
                | "curtime" | "current_time" => match args.first() {
                    Some(Expr::Value(v)) => match &v.value {
                        sqlparser::ast::Value::Number(text, _) => text.parse().unwrap_or(0),
                        _ => 0,
                    },
                    _ => 0,
                },
                // Everything below hands back one of its arguments, so the
                // precision comes from the same argument its *type* does —
                // taken from `computed_column_type`'s own families rather than
                // a second list, because two lists drift. MySQL keeps it:
                // `DATE_ADD(dt3, INTERVAL 1 DAY) + 0` and `MAX(dt3) + 0` are
                // both NEWDECIMAL there and both keep all three digits.
                "date_add" | "date_sub" | "adddate" | "subdate" | "nullif" => first(1),
                // `TIMESTAMPADD(HOUR, 1, dt)` puts the unit first, so the
                // value it shifts is the third argument.
                "timestampadd" => args
                    .get(2)
                    .map_or(0, |a| temporal_precision(a, refs, input, sub)),
                "min" | "max" => {
                    aggregate_argument(f).map_or(0, |a| temporal_precision(&a, refs, input, sub))
                }
                "coalesce" | "ifnull" | "nvl" | "greatest" | "least" => widest(&args),
                // `IF` chooses between its second and third; the condition
                // says nothing about the result.
                "if" => widest(args.get(1..).unwrap_or(&[])),
                _ => 0,
            }
        }
        // A `CASE` hands back one of its results, and any of them can be the
        // one that runs.
        Expr::Case {
            conditions,
            else_result,
            ..
        } => conditions
            .iter()
            .map(|w| temporal_precision(&w.result, refs, input, sub))
            .chain(
                else_result
                    .iter()
                    .map(|e| temporal_precision(e, refs, input, sub)),
            )
            .max()
            .unwrap_or(0),
        _ => 0,
    }
}

/// Whether a code is one MySQL computes over as a string.
///
/// A string has no numeric type of its own, so MySQL converts it to a double
/// and the arithmetic over it is approximate whatever sat on the other side:
/// `t + 0.0` and `SUM(t)` over a CHAR, TEXT, BLOB or JSON column are all
/// DOUBLE on 8.3, not NEWDECIMAL. An ENUM is one of these — its code is
/// `STRING` — which is how `e + 0.0` and `SUM(e)` get MySQL's answer without
/// a rule of their own.
///
/// `VAR_STRING` is deliberately not in the list, and a VARCHAR therefore is
/// not either. It is also `computed_column_type`'s answer for an expression it
/// cannot type at all — a function it does not recognise, an operator with no
/// arm, a scalar subquery nested past the describe limit — so treating it as a
/// string would make every *unknown* operand approximate where the evaluator
/// still produces an exact value for it, and the binary protocol would send
/// eight bytes of a float. Telling a VARCHAR from an unknown needs a code they
/// do not share, which is its own change.
fn is_textual_code(t: ColumnType) -> bool {
    matches!(
        t,
        ColumnType::String
            | ColumnType::Blob
            | ColumnType::TinyBlob
            | ColumnType::MediumBlob
            | ColumnType::LongBlob
            | ColumnType::Json
    )
}

/// Whether a code is one MySQL computes over approximately.
///
/// A FLOAT or a DOUBLE, and the string family that converts to one. Named
/// rather than left inside [`arithmetic_type`] because `/` has to ask this
/// question on its own: that function answers DOUBLE for two different
/// reasons — an operand that *is* approximate, and an operand it could not
/// type at all — and only the first of them makes a quotient approximate.
fn is_approximate_code(t: ColumnType) -> bool {
    matches!(t, ColumnType::Float | ColumnType::Double) || is_textual_code(t)
}

/// The wider of two numeric codes, for an operator with two operands.
fn arithmetic_type(a: ColumnType, b: ColumnType) -> ColumnType {
    let approximate = is_approximate_code;
    let exact = |t: ColumnType| matches!(t, ColumnType::Decimal | ColumnType::NewDecimal);
    if approximate(a) || approximate(b) {
        ColumnType::Double
    } else if exact(a) || exact(b) {
        ColumnType::NewDecimal
    } else if is_integer(a) && is_integer(b) {
        // Two integers of any width give a `LONGLONG`: a sum or a product
        // outgrows both, and the binary protocol would send too few bytes for
        // it under any narrower code.
        ColumnType::LongLong
    } else {
        // An operand of unknown type — a string column, a function this does
        // not recognise, a scalar subquery nested past the describe limit.
        // MySQL answers DOUBLE for arithmetic over a string, and so does the
        // evaluator: `'3.5' + 1` is 4.5. Calling it an integer had the binary
        // protocol send 4.
        ColumnType::Double
    }
}

/// One type for a set of alternatives, the way MySQL aggregates them.
///
/// Used wherever an expression can produce more than one of its parts —
/// `CASE`, `COALESCE`, `IF`, `GREATEST`. Taking the first was the bug: a
/// `CASE` returning a `TINYINT` in one branch and 1000 in another was declared
/// one byte wide, and the binary protocol sent 232.
fn combined(types: impl Iterator<Item = (ColumnType, bool)>) -> ColumnType {
    let mut answer: Option<(ColumnType, bool)> = None;
    for (t, unsigned) in types {
        // NULL says nothing about the others.
        if t == ColumnType::Null {
            continue;
        }
        answer = Some(match answer {
            None => (t, unsigned),
            // The signs are carried and handed to `widen`, which is the only
            // thing that knows a `BIGINT UNSIGNED` beside a `BIGINT` has no
            // integer left to widen into. Passing `false` for both — which
            // this did — made `COALESCE(bu, b)` a signed LONGLONG where MySQL
            // says NEWDECIMAL, and an unsigned value under that code reached a
            // binary client as -1 while the text protocol wrote every digit.
            //
            // The widened code carries the *narrower* sign: an unsigned value
            // widened to hold a negative one is no longer unsigned, and two
            // unsigned operands stay unsigned.
            Some((so_far, so_far_unsigned)) => (
                widen(so_far, so_far_unsigned, t, unsigned),
                so_far_unsigned && unsigned,
            ),
        });
    }
    answer.map(|(t, _)| t).unwrap_or(ColumnType::Null)
}

/// One candidate of a conditional, as the pair [`combined`] folds.
///
/// The sign is asked for only where the code is an integer one, because that
/// is the only place `widen` reads it — and because `unsigned_answer` calls
/// back into `computed_column_type`, so an unconditional call here would make
/// the two walk each other over every subexpression rather than over the
/// integer ones.
fn candidate_type(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
) -> (ColumnType, bool) {
    let ty = computed_column_type(e, refs, input, sub);
    let unsigned = is_integer(ty) && unsigned_answer(e, refs, input, sub);
    (ty, unsigned)
}

/// The declaration a set operation's column takes when both branches are
/// approximate and both declare a scale.
///
/// `None` where either side is a plain `FLOAT`/`DOUBLE`, a computed
/// expression, or anything that is not approximate — MySQL drops to a
/// not-fixed column there, which is what an absent declaration already means.
/// A `DECIMAL` beside an approximate is also left out: 8.3 answers
/// `double(13,4)` for `FLOAT(10,4) UNION ALL DECIMAL(10,3)` and the 13 does
/// not follow from the rule the three approximate pairs below give, so it is
/// not guessed at here.
///
/// The scale is the wider of the two and the width keeps the wider integer
/// part, both measured: `FLOAT(12,2) UNION ALL FLOAT(10,4)` is `float(14,4)`,
/// `FLOAT(12,2) UNION ALL DOUBLE(10,4)` is `double(14,4)`, and
/// `FLOAT(10,4) UNION ALL FLOAT(10,4)` is `float(10,4)` — so ten integer
/// digits beside six give ten, and four places beside two give four.
fn merged_approximate(
    ty: ColumnType,
    left: &Option<crate::types::SqlType>,
    right: &Option<crate::types::SqlType>,
) -> Option<crate::types::SqlType> {
    let name = match ty {
        ColumnType::Float => "float",
        ColumnType::Double => "double",
        _ => return None,
    };
    let parts = |declared: &Option<crate::types::SqlType>| {
        let declared = declared.as_ref()?;
        if declared.kind != crate::types::TypeKind::Float {
            return None;
        }
        Some((declared.length?, declared.scale?))
    };
    let (left_length, left_scale) = parts(left)?;
    let (right_length, right_scale) = parts(right)?;
    let scale = left_scale.max(right_scale);
    let integral = left_length
        .saturating_sub(left_scale)
        .max(right_length.saturating_sub(right_scale));
    let length = integral.saturating_add(scale);
    Some(crate::types::SqlType {
        kind: crate::types::TypeKind::Float,
        declared: format!("{name}({length},{scale})"),
        length: Some(length),
        scale: Some(scale),
        unsigned: false,
        variants: Vec::new(),
        binary_collation: false,
        collation: crate::collation::Collation::Ai0900Ci,
    })
}

/// What a computed column is described with beyond its type code: MySQL's
/// `Item::max_length` and `Item::decimals`.
///
/// Both are read by a client to decide how to decode — and, for a generated
/// one, how to *type* — the values that follow, which is why a `DECIMAL`
/// column described with no places is a wrong answer even where the digits
/// that arrive are right. #0111.
///
/// `None` in either field means "not modelled", and reads as the type's
/// default: 20 characters and no places for a number, 4096 for a string. That
/// is the answer this engine gave for every expression before this walk
/// existed, so a shape left out here keeps the old answer rather than
/// inventing a new one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ItemShape {
    max_length: Option<u32>,
    decimals: Option<u8>,
}

/// The width and the places to describe an expression with.
///
/// Dispatched on the type code the column is going out under, because that is
/// what decides which arithmetic applies — MySQL settles an item's type first
/// and sizes it second, and so does this.
fn item_shape(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
) -> ItemShape {
    let ty = computed_column_type(e, refs, input, sub);
    // Asked once here rather than once per node: `computed_column_type` runs
    // a probe query for a scalar subquery, so asking it once per expression is
    // the cost this is worth and asking it once per *node* is not. The walks
    // below dispatch on the shape of the tree instead.
    if is_integer(ty) {
        return ItemShape {
            max_length: printed_width(e, refs, input),
            decimals: Some(0),
        };
    }
    match ty {
        ColumnType::NewDecimal | ColumnType::Decimal => {
            let exact = exact_shape(e, refs, input, sub);
            ItemShape {
                max_length: exact.length,
                decimals: exact.scale.map(places),
            }
        }
        // An approximate number has no fixed places unless a `FLOAT(M,D)` or
        // `DOUBLE(M,D)` puts some on it, and a declared scale propagates
        // through arithmetic exactly as an exact one does — measured on 8.3,
        // `fm + fm` and `SUM(fm)` over a `FLOAT(10,4)` both still show four
        // places and `fm / 2` shows eight, which is the increment again.
        //
        // The *width* of one that does is deliberately not modelled. It is not
        // the exact arithmetic: an approximate item's width has no column for
        // the point or the sign and gains none for a carry, so `fm + fm` is 10
        // on 8.3 where the same shape read exactly would be 11, and `SUM(fm)`
        // is 21 against `SUM`'s exact 32. Carrying `exact_shape`'s number here
        // would be a measured-wrong answer where `None` is the type's default
        // and the answer this engine already gave.
        ColumnType::Float | ColumnType::Double => {
            let approximate = exact_shape(e, refs, input, sub);
            match approximate.scale {
                Some(scale) => ItemShape {
                    max_length: None,
                    decimals: Some(places(scale)),
                },
                None => ItemShape {
                    max_length: unfixed_real_width(e, ty),
                    decimals: Some(mysql_ultralight_protocol::column::NOT_FIXED_DEC),
                },
            }
        }
        // A temporal's places are its fractional seconds, which is the same
        // number `arithmetic_operand_type` reads to decide whether arithmetic
        // over it is exact. Measured on 8.3: `DATETIME(3)` is `dec 3`,
        // `TIME(6)` is 6, `CAST(dt3 AS DATETIME(6))` is 6 and `SEC_TO_TIME(1)`
        // is 0.
        t if is_temporal(t) => ItemShape {
            max_length: None,
            decimals: u8::try_from(temporal_precision(e, refs, input, sub)).ok(),
        },
        // Every string an *item* produces reports `NOT_FIXED_DEC`, where a
        // declared string column reports 0 — measured both ways, and the split
        // is a field against an item rather than anything about the text.
        //
        // A width is what says this really is a string. `VAR_STRING` is also
        // `computed_column_type`'s answer for an expression it cannot type at
        // all — see `is_textual_code` — so claiming 31 places on the strength
        // of the code alone would describe an unknown as a string. A document
        // is left out for the same reason in reverse: `JSON_OBJECT` has a
        // width this walk does not model, and 31 for it is unmeasured.
        t if is_textual_code(t) || t == ColumnType::VarString => {
            match string_width(e, refs, input, sub) {
                Some(bytes) => ItemShape {
                    max_length: Some(wire_string_width(bytes, binary_answer(e, refs, sub))),
                    decimals: Some(mysql_ultralight_protocol::column::NOT_FIXED_DEC),
                },
                None => ItemShape::default(),
            }
        }
        // A materialised NULL has no width at all: `SELECT NULL` is `len 0` on
        // 8.3 where this engine sent the numeric default of 20.
        ColumnType::Null => ItemShape {
            max_length: Some(0),
            decimals: Some(0),
        },
        _ => ItemShape::default(),
    }
}

/// A scale as the wire's `decimals`, saturating at what a byte holds.
///
/// Nothing reaches 255: MySQL's own cap is 30 places for an exact number and
/// 31 for `NOT_FIXED_DEC`. The saturation is here so a bug in the arithmetic
/// above cannot wrap a wide scale round to a narrow one, which would read as
/// a plausible answer.
fn places(scale: u32) -> u8 {
    u8::try_from(scale).unwrap_or(u8::MAX)
}

/// The width of an approximate number with no fixed places.
///
/// Measured on 8.3, and it is a width per *type* rather than per expression:
/// every DOUBLE-valued operator, aggregate and function reports 23 — `f + f`,
/// `SUM(dd)`, `AVG(dd)`, `dd / 2`, `ROUND(dd, 2)`, `ABS(f)`, `-dd`,
/// `CAST(dd AS DOUBLE)`, `POW(a, 2)` and `SQRT(a)` all do — and a FLOAT-valued
/// one reports 12, which is what `MIN(f)` over a `FLOAT` answers.
///
/// A *literal* is the exception and is deliberately left unmodelled: `1.5e0` is
/// 5 on 8.3, its own digits, and there is no reason to think the rule for one
/// is the rule for the other. `None` keeps the type's default.
fn unfixed_real_width(e: &Expr, ty: ColumnType) -> Option<u32> {
    if matches!(e, Expr::Value(_)) {
        return None;
    }
    Some(if ty == ColumnType::Float { 12 } else { 23 })
}

/// The width a computed string goes out with, from the bytes its widest value
/// can hold.
///
/// MySQL sends `Item::max_length` for a VARCHAR-coded item and four times it —
/// the connection character set's bytes per character — for a blob-coded one,
/// which is why `UPPER(tx)` over a utf8mb4 `TEXT` is 1048560 and not the
/// 262140 the arithmetic produced. Measured on 8.3 across both sides of the
/// threshold: `HEX(a)` over a `CHAR(255)` is 8160 and `HEX(b)` over a
/// `VARCHAR(16000)` is 2048000, which is four times its own 512000.
///
/// Not for an answer that is bytes, which is not converted into anything on
/// the way out: `UNHEX(tx)` over a `TEXT` is 131070 on 8.3, its own arithmetic
/// and not four times it, where `TO_BASE64(tx)` beside it — the same width
/// class, characters rather than bytes — is multiplied.
///
/// What this cannot follow is a *text* answer MySQL calls binary by its
/// operand's collation: `CONCAT(bl, bl)` over a `BLOB` is 131070 there and
/// 524280 here, because this engine describes it as characters. That is the
/// same collation gap `OutColumn::to_wire` records and not a second one.
fn wire_string_width(bytes: u32, binary: bool) -> u32 {
    if binary || string_code_for(bytes) == ColumnType::VarString {
        return bytes;
    }
    bytes.saturating_mul(4)
}

/// The type code a computed string of this many bytes goes out under.
///
/// MySQL picks a string item's code from its width, not from what produced it,
/// which is why the same function answers three different codes over three
/// different columns. Measured on 8.3, over utf8mb4:
///
/// | argument | `UPPER` of it | `HEX` of it |
/// |---|---|---|
/// | `CHAR(10)`, `VARCHAR(20)`, `TINYTEXT`, `ENUM` | VAR_STRING | VAR_STRING |
/// | `TINYBLOB` | — | VAR_STRING |
/// | `TEXT` | MEDIUM_BLOB | MEDIUM_BLOB |
/// | `BLOB` | — | MEDIUM_BLOB |
/// | `MEDIUMTEXT`, `LONGTEXT`, `JSON` | LONG_BLOB | LONG_BLOB |
/// | `MEDIUMBLOB` | — | LONG_BLOB |
///
/// The two thresholds are the widths a `VARCHAR` and a `MEDIUMBLOB` hold. A
/// TINY_BLOB or a plain BLOB code is never reached, measured: an item that
/// outgrows a VARCHAR at all goes straight to MEDIUM_BLOB. #0142.
fn string_code_for(bytes: u32) -> ColumnType {
    /// The widest a `VARCHAR` is, and therefore the widest an item MySQL
    /// describes as one can be.
    const VARCHAR_MAX: u32 = 65535;
    /// The widest a `MEDIUMBLOB` holds.
    const MEDIUM_MAX: u32 = 16777215;
    if bytes <= VARCHAR_MAX {
        ColumnType::VarString
    } else if bytes <= MEDIUM_MAX {
        ColumnType::MediumBlob
    } else {
        ColumnType::LongBlob
    }
}

/// Whether a function's result width — and therefore its type code — is sized
/// from its arguments.
///
/// The list is measured rather than reasoned about, and the exclusions are the
/// point: `MD5` is 32 characters whatever it hashed, `SPACE` and `BIN` have
/// widths of their own, and `GROUP_CONCAT` is a LONG_BLOB by
/// `group_concat_max_len` rather than by its argument — see the note where
/// `function_result_type` names it.
fn widens_with_argument(name: &str) -> bool {
    matches!(
        name,
        "concat"
            | "concat_ws"
            | "upper"
            | "ucase"
            | "lower"
            | "lcase"
            | "ltrim"
            | "rtrim"
            | "mid"
            | "left"
            | "right"
            | "replace"
            | "reverse"
            | "lpad"
            | "rpad"
            | "hex"
            | "unhex"
            | "to_base64"
            | "from_base64"
    )
}

/// The type code a string expression takes, from the width of its answer.
///
/// `VAR_STRING` where the width is not modelled, which is what this engine
/// answered for every string function before #0142 — so a function whose
/// arithmetic is missing keeps the old answer rather than getting a guessed
/// one.
fn widened_string_type(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
) -> ColumnType {
    string_width(e, refs, input, sub).map_or(ColumnType::VarString, string_code_for)
}

/// How many bytes a computed string's widest value can hold, in the result's
/// character set.
///
/// MySQL's `Item::max_length` for a string, which is what decides both the
/// width on the wire and the type code — see [`string_code_for`]. The
/// arithmetic is per function and is measured, over a `CHAR(10)`, a `TEXT`
/// `tx` and a `BLOB` `bl` on 8.3:
///
/// | expression | characters | bytes |
/// |---|---|---|
/// | `'x'` | 1 | 4 |
/// | `UPPER(c)`, `LTRIM(tx)`, `REVERSE(tx)` | the argument's | ×4 |
/// | `CONCAT(c, 'x')` | 10 + 1 | 44 |
/// | `LEFT(tx, 2)`, `LPAD(tx, 5, 'x')` | the count asked for | 8, 20 |
/// | `HEX(c)` | twice the argument's **bytes** | 320 |
///
/// `HEX` is the one that has to be read carefully: it doubles the bytes, not
/// the characters, so over a utf8mb4 `CHAR(10)` it is 80 characters and not
/// 20. That is what makes `HEX(bl)` over a `BLOB` a MEDIUM_BLOB where
/// `HEX(tb)` over a `TINYBLOB` is a VAR_STRING.
///
/// Everything is counted in characters and multiplied at the end, because this
/// engine speaks one character set: a character is four bytes here, always. It
/// is the same constant `SqlType::wire_length` uses and it stops being one on
/// the day a second character set exists.
///
/// `None` is "not modelled", and it is also the answer for an expression that
/// may not be a string at all — `VAR_STRING` is `computed_column_type`'s reply
/// for anything it cannot type, so a width is the evidence that this really is
/// one.
fn string_width(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
) -> Option<u32> {
    /// utf8mb4, the only character set this engine has.
    const BYTES_PER_CHAR: u32 = 4;
    let counted = string_chars(e, refs, input)?;
    // An answer that is bytes is already in bytes, and multiplying it would
    // widen a `VARBINARY` into a blob: `UNHEX(bl)` over a `BLOB` is a
    // VAR_STRING of 32768 on 8.3, where four times that would have crossed
    // into MEDIUM_BLOB. `binary_answer` is the same question `binary_values`
    // asks, so the width and the charset cannot disagree about one column.
    if binary_answer(e, refs, sub) {
        return Some(counted);
    }
    Some(counted.saturating_mul(BYTES_PER_CHAR))
}

/// The character count behind [`string_width`], before the character set's
/// multiplier is applied.
///
/// No `Sub`, unlike the walks around it, because nothing here resolves a
/// scalar subquery: `(SELECT tx)` is unmodelled and keeps the old answer. 8.3
/// describes one as the *field* behind it — `BLOB len 262140` over a `TEXT`
/// column, with the BLOB flag a function's answer does not carry — which is a
/// different rule from the one this file models and not #0142's.
fn string_chars(e: &Expr, refs: &[ColumnRef], input: &[crate::relation::RelColumn]) -> Option<u32> {
    let recur = |inner: &Expr| string_chars(inner, refs, input);
    // The bytes an operand holds, which is what `HEX` doubles. A text column's
    // wire width is already in bytes and a binary one's is bytes by
    // definition, so this is the character count times the multiplier again
    // only for the textual kinds.
    let bytes = |inner: &Expr| -> Option<u32> {
        let chars = string_chars(inner, refs, input)?;
        let textual = match inner {
            Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
                direct_column(inner, refs, input).is_some_and(|column| column.ty.is_textual())
            }
            _ => true,
        };
        Some(if textual {
            chars.saturating_mul(4)
        } else {
            chars
        })
    };
    match e {
        Expr::Nested(inner) => recur(inner),
        // A column's own width, in the units MySQL counts it in: characters
        // for text, bytes for a BLOB — which is what `wire_length` already
        // answers, divided back out for the textual kinds.
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            let column = direct_column(e, refs, input)?;
            let width = column.ty.wire_length();
            Some(if column.ty.is_textual() {
                width / 4
            } else {
                width
            })
        }
        Expr::Value(v) => match &v.value {
            sqlparser::ast::Value::SingleQuotedString(text)
            | sqlparser::ast::Value::DoubleQuotedString(text) => {
                u32::try_from(text.chars().count()).ok()
            }
            // A NULL contributes nothing to a width, which is `Item_null`'s
            // own zero.
            sqlparser::ast::Value::Null => Some(0),
            _ => None,
        },
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_lowercase();
            let args = function_arguments(f);
            let first = args.first()?;
            match name.as_str() {
                // The shape-preserving ones: the answer is at most as long as
                // what it was given.
                "upper" | "ucase" | "lower" | "lcase" | "ltrim" | "rtrim" | "reverse" => {
                    recur(first)
                }
                // A replacement longer than what it replaces makes the answer
                // longer, and by a rule that reads oddly and is measured
                // rather than derived: the width is the subject's *times one
                // less than the replacement*, and the pattern's own length
                // does not enter it. Over a `CHAR(10)` on 8.3, `REPLACE(c,
                // 'a', 'zz')` is 40 bytes, `'zzz'` is 80, `'zzzz'` is 120 and
                // a `VARCHAR(20)` replacement is 760 — and swapping the
                // pattern for `'ab'`, `'abc'` or `'abcd'` changes none of
                // them. Thirteen shapes measured, over a `CHAR(4)` as well as
                // a `CHAR(10)`.
                //
                // Only where the answer can grow at all: an empty pattern and
                // a replacement no longer than the pattern both leave the
                // subject's own width, measured.
                "replace" => {
                    let subject = recur(first)?;
                    let pattern = args.get(1).and_then(&recur)?;
                    let replacement = args.get(2).and_then(&recur)?;
                    if pattern == 0 || replacement <= pattern {
                        return Some(subject);
                    }
                    Some(subject.saturating_mul(replacement - 1))
                }
                // Everything joined, and for `CONCAT_WS` the separator once
                // between each pair.
                "concat" => args.iter().try_fold(0u32, |total, argument| {
                    Some(total.saturating_add(recur(argument)?))
                }),
                "concat_ws" => {
                    let joined = args[1..].iter().try_fold(0u32, |total, argument| {
                        Some(total.saturating_add(recur(argument)?))
                    })?;
                    let separators = u32::try_from(args.len().saturating_sub(2)).ok()?;
                    Some(joined.saturating_add(recur(first)?.saturating_mul(separators)))
                }
                // A count asked for in the statement, which cannot be more
                // than what there is to take.
                "left" | "right" => match args.get(1).and_then(literal_scale) {
                    Some(count) if count >= 0 => {
                        Some(u32::try_from(count).ok()?.min(recur(first)?))
                    }
                    _ => recur(first),
                },
                // `MID` is `SUBSTRING` under another name, so its second
                // argument is a *position* and its third is the length —
                // reading the second as a count made `MID(tx, 2, 5)` eight
                // bytes wide where `SUBSTRING(tx, 2, 5)` beside it is twenty,
                // the same call answering two things. Measured on 8.3: the two
                // spellings agree on both, including `MID(tx, 2)`, which is a
                // MEDIUM_BLOB.
                "mid" => Some(sliced(
                    recur(first)?,
                    args.get(1).and_then(literal_scale),
                    args.get(2).and_then(literal_scale),
                )),
                // A pad is exactly the width asked for.
                "lpad" | "rpad" => match args.get(1).and_then(literal_scale) {
                    Some(count) if count >= 0 => u32::try_from(count).ok(),
                    _ => None,
                },
                // Two characters per byte, and the inverse for the two that
                // read one back. Every number measured on 8.3 over a
                // `CHAR(10)` (40 bytes), a `TEXT` (262140) and a `BLOB`
                // (65535):
                //
                // | call | bytes |
                // |---|---|
                // | `HEX(c)`, `HEX(tx)` | 320, 2097120 |
                // | `UNHEX(c)`, `UNHEX(bl)` | 20, 32768 |
                // | `TO_BASE64(c)`, `TO_BASE64(bl)` | 224, 1416464 |
                // | `FROM_BASE64(c)`, `FROM_BASE64(bl)` | 30, 49151 |
                //
                // `UNHEX` rounds *up*, which is what makes a `BLOB` 32768
                // rather than 32767 — and the difference decides a type code,
                // since 32768 characters of bytes still fit a VARCHAR.
                "hex" => Some(bytes(first)?.saturating_mul(2)),
                "unhex" => Some(bytes(first)?.div_ceil(2)),
                // Four characters per three bytes, plus a newline every
                // seventy-six of them. The wrapping is not decoration: it is
                // the difference between 1416464 and 1398144 for a `BLOB`.
                "to_base64" => {
                    let encoded = bytes(first)?.div_ceil(3).saturating_mul(4);
                    Some(encoded.saturating_add(encoded / 76))
                }
                "from_base64" => Some(bytes(first)?.saturating_mul(3) / 4),
                _ => None,
            }
        }
        // `SUBSTRING` has its own grammar. With a length it is that length;
        // with only a start it is what is left after it — measured,
        // `SUBSTRING(tx, 2)` over a `TEXT` is one character narrower than `tx`.
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            ..
        } => Some(sliced(
            recur(expr)?,
            substring_from.as_deref().and_then(literal_scale),
            substring_for.as_deref().and_then(literal_scale),
        )),
        Expr::Trim { expr, .. } => recur(expr),
        _ => None,
    }
}

/// How wide a slice of a string can be, for the `SUBSTRING` family.
///
/// With a length it is that length, which cannot be more than there is to
/// take; with only a start it is what is left after it — measured on 8.3,
/// `SUBSTRING(tx, 2)` over a `TEXT` is one character narrower than `tx`, which
/// is what keeps it a MEDIUM_BLOB where `SUBSTRING(tx, 1, 3)` is a VAR_STRING.
/// A start or a length that is not a literal is not known here, and the whole
/// width stands.
fn sliced(whole: u32, start: Option<i64>, length: Option<i64>) -> u32 {
    if let Some(length) = length.filter(|length| *length >= 0) {
        return u32::try_from(length).unwrap_or(u32::MAX).min(whole);
    }
    match start.filter(|start| *start >= 1) {
        Some(start) => whole.saturating_sub(u32::try_from(start - 1).unwrap_or(u32::MAX)),
        None => whole,
    }
}

/// An expression's width and places, in the two numbers MySQL keeps on an
/// item: `max_length` counts the characters the widest value prints — the
/// digits, a column for the point where there is a scale, and a column for the
/// sign where the value can be negative — and `decimals` counts the places.
///
/// The *precision* is not a third field, because it is not independent:
/// MySQL's `my_decimal_length_to_precision` recovers it from these two and the
/// sign, and carrying it separately is how the width on the wire and the width
/// in a view come to disagree.
///
/// Either half may be unknown on its own, which is what lets a shape be
/// partly modelled. A `DATETIME(3)` in arithmetic is the case that needs it:
/// its places are its fractional seconds — `dt3 + 0` shows three on 8.3 — and
/// its width is the *packed* number's, which is not its display width and is
/// not measured here. So the scale travels and the width does not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Exact {
    length: Option<u32>,
    scale: Option<u32>,
}

impl Exact {
    /// The scale is clamped here and nowhere else, so every shape this walk
    /// produces carries a number of places MySQL can hold. It is not only a
    /// guard: MySQL clamps on the *item*, so the digits before the point widen
    /// to match — a `DECIMAL(40,20)` squared is `decimal(65,30)` there, and 35
    /// integral digits is what the next operator over it reads.
    ///
    /// The width is unaffected either way, because only *whether* there are
    /// places reaches it — see `exact_length`.
    ///
    /// The one thing that can arrive over the cap is a literal: nothing stops
    /// a client writing forty digits after a point.
    fn new(length: u32, scale: u32) -> Self {
        Self {
            length: Some(length),
            scale: Some(scale.min(MAX_SCALE)),
        }
    }

    /// The places alone, for an operand whose width is not modelled.
    fn scale_only(scale: u32) -> Self {
        Self {
            length: None,
            scale: Some(scale.min(MAX_SCALE)),
        }
    }
}

/// MySQL's largest exact precision, which every rule below clamps to.
/// Measured on 8.3 over two `DECIMAL(40,20)` columns: the product is
/// `decimal(65,30)` where the arithmetic asks for 80 and 40.
///
/// The matching cap on the *scale* is not here, because the scale rules are
/// not here: `crate::decimal` owns them, and the clamp travels with each.
const MAX_PRECISION: u32 = 65;

/// The scale rules, read from the module that owns them.
///
/// A sum's, a product's and a quotient's number of places is one rule with two
/// readers — the scale a value is *shown* at and the scale its column is
/// *described* with are the same number, and #0118 measured it for the first.
/// These wrappers exist only to move between `usize` and the `u32` this walk
/// counts digits in; the rules themselves are in one place, so a measurement
/// that corrects one corrects both.
fn widest_scale(a: u32, b: u32) -> u32 {
    crate::decimal::widest_display_scale(a as usize, b as usize) as u32
}

fn product_scale(a: u32, b: u32) -> u32 {
    crate::decimal::product_display_scale(a as usize, b as usize) as u32
}

fn quotient_scale(a: u32, increment: u32) -> u32 {
    crate::decimal::quotient_display_scale(a as usize, increment as usize) as u32
}

/// The widest scale any of those rules can answer, for the places a shape is
/// carried at where no rule applies.
const MAX_SCALE: u32 = crate::decimal::MAX_SCALE as u32;

/// The digits an item can hold, recovered from its width and its places.
///
/// `None` where the two do not describe an exact number between them, which is
/// where the whole walk stops rather than carrying a shape it cannot mean. Two
/// ways in: a width too narrow for its own point and sign, and a *scale wider
/// than the precision*, which `NOT_FIXED_DEC` is — 31 places against the 21
/// digits a DOUBLE's width leaves. Every rule below subtracts one of these
/// from the other, so an impossible pair is checked here once instead of at
/// each of them, and `ROUND` over an approximate scalar subquery is the shape
/// that found it.
fn exact_precision(shape: Exact, unsigned: bool) -> Option<u32> {
    let scale = shape.scale?;
    let precision = shape
        .length?
        .checked_sub(u32::from(scale > 0) + u32::from(!unsigned))?;
    (precision >= scale).then_some(precision)
}

/// The digits *before* the point, which is what the additive and conditional
/// rules take the widest of.
fn exact_integral(shape: Exact, unsigned: bool) -> Option<u32> {
    exact_precision(shape, unsigned)?.checked_sub(shape.scale?)
}

/// The width a precision and a scale print in, which is `exact_precision`
/// backwards.
fn exact_length(precision: u32, scale: u32, unsigned: bool) -> u32 {
    precision
        .saturating_add(u32::from(scale > 0))
        .saturating_add(u32::from(!unsigned))
}

/// The width and places an expression that computes exactly is described with.
///
/// Every rule measured against 8.3 rather than read, over a `DECIMAL(10,2)`
/// `a`, a `DECIMAL(7,3)` `b` and a `DECIMAL(6,2) UNSIGNED` `u`:
///
/// | expression | precision | scale | width |
/// |---|---|---|---|
/// | `a`, `b` | 10, 7 | 2, 3 | 12, 9 |
/// | `a + b`, `a - b` | max integral + 1 + scale = 12 | max = 3 | 14 |
/// | `a * b` | 10 + 7 = 17 | 2 + 3 = 5 | 19 |
/// | `a / b` | 10 + 3 + increment = 17 | 2 + increment = 6 | 19 |
/// | `a MOD b` | from the wider *width* | max = 3 | max(12, 9) = 12 |
/// | `SUM(a)` | 10 + 22 = 32 | 2 | 34 |
/// | `AVG(a)` | 10 + increment = 14 | 2 + increment = 6 | 16 |
/// | `MIN(a)`, `MAX(a)`, `ABS(a)` | 10 | 2 | 12 |
/// | `ROUND(a, 1)` | 10 - 1 + 1 = 10 | 1 | 12 |
/// | `TRUNCATE(a, 1)` | 10 - 2 + 1 = 9 | 1 | 11 |
/// | `GREATEST(a, b)` | max integral + scale = 11 | max = 3 | 13 |
///
/// The two asymmetries are the ones worth having written down. Addition does
/// *not* clamp its precision where multiplication does — `DECIMAL(65,30) +
/// DECIMAL(3,3)` is 68 wide on 8.3, a precision of 66 — and `MOD` works on the
/// two *widths* rather than on the two precisions, which is the only way to
/// get its measured answer for `a MOD b`: the precisions give 8 + 3 = 11 and a
/// width of 13, and MySQL says 12.
///
/// An operand whose shape is not modelled makes the answer not modelled, which
/// is what keeps this a subset: `None` reads as the type's default width, the
/// answer the engine already gave.
fn exact_shape(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
) -> Exact {
    use sqlparser::ast::BinaryOperator as B;
    use sqlparser::ast::UnaryOperator as U;

    /// What `SUM` adds to its argument's precision, `DECIMAL_LONGLONG_DIGITS`
    /// in the server: measured, `SUM` over a `DECIMAL(10,2)` is 34 wide and
    /// over an `INT` 33.
    const SUM_DIGITS: u32 = 22;

    let recur = |inner: &Expr| exact_shape(inner, refs, input, sub);
    // Read once, on the paths that need it. Every walker here is recursive, so
    // a question asked at each node about that node's whole subtree is asked a
    // quadratic number of times; asking it lazily keeps it off the arms that
    // answer "not modelled" and walk no further.
    let signed_here = || !unsigned_answer(e, refs, input, sub);
    // The width of a precision and a scale under *this* node's sign, which is
    // where a negation gets its extra column: `-u` over an unsigned
    // `DECIMAL(6,2)` is 8 wide on 8.3 against the column's own 7.
    //
    // The scale arrives already clamped, by whichever rule produced it. Only
    // *whether there are any* places reaches the width, so the clamp cannot
    // move it: 8.3 reports `DECIMAL(40,20) * DECIMAL(40,20)` as `len 67
    // dec 30`, and 65 digits with a point and a sign is 67 at either scale.
    let sized = |precision: u32, scale: u32| {
        Exact::new(exact_length(precision, scale, !signed_here()), scale)
    };
    // The parts of one operand, under *its* own sign rather than the node's:
    // `u + u` is unsigned and `a + u` is not, and each operand's precision has
    // to be read the way that operand prints.
    //
    // Takes the shape already walked rather than walking it again. It used to
    // take the expression, and the arms below then walked each child twice —
    // once here and once for the scale in the fallback — which is 2^n over a
    // chain of operators whose shape is not modelled. Measured before the fix:
    // twenty `+ 0` terms over a `DATETIME(3)` column took 2.3 seconds to
    // describe, and twenty-two took eight.
    let parts = |inner: &Expr, shape: Exact| {
        let unsigned = unsigned_answer(inner, refs, input, sub);
        Some((
            exact_precision(shape, unsigned)?,
            exact_integral(shape, unsigned)?,
            shape.scale?,
        ))
    };
    // The corrected shape, for the reason `printed_width` asks for it: the
    // operator at the root as parsed is often not the operator that decides,
    // and a walk that reads a different tree than `eval` describes a different
    // expression than the one that runs.
    if let Some(fixed) = crate::expr::reassociated_postfix(e) {
        return exact_shape(&fixed, refs, input, sub);
    }
    match e {
        Expr::Nested(inner) => recur(inner),
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            let Some(column) = direct_column(e, refs, input) else {
                return Exact::default();
            };
            let ty = &column.ty;
            match ty.kind {
                // A declared scale, or none at all for a `FLOAT` that has one
                // — the approximate arm in `item_shape` reads the difference.
                crate::types::TypeKind::Decimal => {
                    Exact::new(ty.wire_length(), ty.scale.unwrap_or(0) as u32)
                }
                crate::types::TypeKind::Float => match ty.scale {
                    Some(scale) => Exact::new(ty.wire_length(), scale as u32),
                    None => Exact::default(),
                },
                crate::types::TypeKind::Int
                | crate::types::TypeKind::Bool
                | crate::types::TypeKind::Year => Exact::new(ty.wire_length(), 0),
                // A temporal computes in its packed form, whose width is not
                // its display width — see the note on `Exact` — so only the
                // places travel.
                crate::types::TypeKind::DateTime | crate::types::TypeKind::Time => {
                    Exact::scale_only(ty.length.unwrap_or(0) as u32)
                }
                crate::types::TypeKind::Date => Exact::scale_only(0),
                _ => Exact::default(),
            }
        }
        // A scalar subquery is described exactly as the column behind it is,
        // and reading it back off the wire definition is what keeps the two
        // from drifting: `(SELECT a FROM T) + 1` sizes from `a`'s own 12 and 2.
        Expr::Subquery(query) => match subquery_column(sub, query, refs) {
            // A column with a *fixed* number of places only. `NOT_FIXED_DEC`
            // is an answer and not a scale — an approximate or string column
            // reports it — and reading it as one gave `(SELECT AVG(d) FROM T)`
            // over a `DOUBLE` a shape of 21 digits and 31 places, which is not
            // a number. 8.3 describes both it and `ROUND` of it `len 23
            // dec 31`, which is what the approximate arm answers once this
            // hands back nothing.
            Some(column) => match column.to_wire() {
                def if u32::from(def.decimals) <= MAX_SCALE => {
                    Exact::new(def.column_length, u32::from(def.decimals))
                }
                _ => Exact::default(),
            },
            None => Exact::default(),
        },
        Expr::Value(v) => match &v.value {
            // The digits as written, plus a column for the sign. The *token*
            // is what counts and not the value: `0.25` is 5 wide on 8.3 and
            // `.25` would be 4, because the leading zero is a digit MySQL was
            // given. An exponent is not an exact literal at all.
            sqlparser::ast::Value::Number(text, _) => {
                let digits = text.strip_prefix('-').unwrap_or(text);
                let (whole, fraction) = match digits.split_once('.') {
                    Some((whole, fraction)) => (whole, fraction),
                    None => (digits, ""),
                };
                let numeric = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
                if whole.is_empty() && fraction.is_empty() || !numeric(whole) || !numeric(fraction)
                {
                    return Exact::default();
                }
                let width = u32::try_from(digits.len()).ok();
                let scale = u32::try_from(fraction.len()).ok();
                match (width, scale) {
                    (Some(width), Some(scale)) => Exact::new(width.saturating_add(1), scale),
                    _ => Exact::default(),
                }
            }
            sqlparser::ast::Value::Boolean(_) => Exact::new(1, 0),
            _ => Exact::default(),
        },
        // The target names the shape outright. `DECIMAL` with nothing after it
        // is `DECIMAL(10,0)`, which is MySQL's default and measures 11 wide.
        Expr::Cast {
            data_type:
                sqlparser::ast::DataType::Decimal(info)
                | sqlparser::ast::DataType::Numeric(info)
                | sqlparser::ast::DataType::Dec(info),
            ..
        } => {
            let (precision, scale) = match info {
                sqlparser::ast::ExactNumberInfo::None => (10, 0),
                sqlparser::ast::ExactNumberInfo::Precision(p) => (*p as u32, 0),
                sqlparser::ast::ExactNumberInfo::PrecisionAndScale(p, s) => (*p as u32, *s as u32),
            };
            sized(precision.min(MAX_PRECISION), scale.min(MAX_SCALE))
        }
        Expr::UnaryOp { op, expr } => {
            if let Some(fixed) = crate::expr::reassociated_unary(op, expr) {
                return exact_shape(&fixed, refs, input, sub);
            }
            match op {
                U::Plus | U::Minus => {
                    let shape = recur(expr);
                    match parts(expr, shape) {
                        Some((precision, _, scale)) => sized(precision, scale),
                        None => scale_only_or_default(shape.scale),
                    }
                }
                _ => Exact::default(),
            }
        }
        Expr::BinaryOp { left, op, right } => {
            if let Some(fixed) = crate::expr::reassociated(op, left, right) {
                return exact_shape(&fixed, refs, input, sub);
            }
            // Each child walked once, whatever the arm below does with it.
            // Two of them fall back to the scales alone, and reading those off
            // a second walk is what made a chain of unmodelled operators cost
            // exponential time.
            let (left_shape, right_shape) = (recur(left), recur(right));
            let scales = || Some(left_shape.scale?.max(right_shape.scale?));
            match op {
                B::Plus | B::Minus => {
                    match (parts(left, left_shape), parts(right, right_shape)) {
                        (
                            Some((_, left_integral, left_scale)),
                            Some((_, right_integral, right_scale)),
                        ) => {
                            let scale = widest_scale(left_scale, right_scale);
                            // One more than the wider integral part, for the
                            // carry. Deliberately unclamped: 8.3 reports a
                            // precision of 66 for `DECIMAL(65,30) +
                            // DECIMAL(3,3)`, past its own maximum.
                            let precision = left_integral.max(right_integral) + 1 + scale;
                            sized(precision, scale)
                        }
                        _ => scale_only_or_default(scales()),
                    }
                }
                B::Multiply => match (parts(left, left_shape), parts(right, right_shape)) {
                    (
                        Some((left_precision, _, left_scale)),
                        Some((right_precision, _, right_scale)),
                    ) => sized(
                        (left_precision + right_precision).min(MAX_PRECISION),
                        product_scale(left_scale, right_scale),
                    ),
                    _ => Exact::default(),
                },
                B::Divide => {
                    let increment = u32::try_from(div_precision_increment()).unwrap_or(0);
                    match (parts(left, left_shape), parts(right, right_shape)) {
                        (Some((left_precision, _, left_scale)), Some((_, _, right_scale))) => {
                            sized(
                                (left_precision + right_scale + increment).min(MAX_PRECISION),
                                quotient_scale(left_scale, increment),
                            )
                        }
                        _ => Exact::default(),
                    }
                }
                // The wider of the two *widths*, converted back to a precision
                // under this node's sign. See the note above.
                B::Modulo => match (left_shape, right_shape) {
                    (
                        Exact {
                            length: Some(left_length),
                            scale: Some(left_scale),
                        },
                        Exact {
                            length: Some(right_length),
                            scale: Some(right_scale),
                        },
                    ) => Exact::new(
                        left_length.max(right_length),
                        widest_scale(left_scale, right_scale),
                    ),
                    _ => scale_only_or_default(scales()),
                },
                _ => Exact::default(),
            }
        }
        // The conditional family builds a new item out of several candidates
        // and takes the widest integral part and the widest scale, with no
        // carry — `GREATEST(a, b)` is 13 wide where `a + b` is 14.
        Expr::Case {
            conditions,
            else_result,
            ..
        } => {
            let mut candidates: Vec<Expr> = conditions.iter().map(|w| w.result.clone()).collect();
            candidates.extend(else_result.as_deref().cloned());
            widest_candidate(&candidates, refs, input, sub, signed_here())
        }
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_lowercase();
            let args = function_arguments(f);
            let increment = || u32::try_from(div_precision_increment()).unwrap_or(0);
            // Walked once and handed to whichever arm wants it, for the
            // reason the binary operators above do the same.
            let folded = aggregate_argument(f);
            let folded_shape = folded.as_ref().map(&recur);
            let folded_parts = || {
                let argument = folded.as_ref()?;
                parts(argument, folded_shape?)
            };
            match name.as_str() {
                // A total needs room for every row it adds up.
                "sum" => match folded_parts() {
                    Some((precision, _, scale)) => {
                        sized((precision + SUM_DIGITS).min(MAX_PRECISION), scale)
                    }
                    None => scale_only_or_default(folded_shape.and_then(|shape| shape.scale)),
                },
                // An average gains the same places a quotient does, because it
                // is one.
                "avg" => match folded_parts() {
                    Some((precision, _, scale)) => sized(
                        (precision + increment()).min(MAX_PRECISION),
                        quotient_scale(scale, increment()),
                    ),
                    None => Exact::default(),
                },
                // These hand one of their operands back rather than computing
                // a new number, so the shape is the operand's — under this
                // node's sign, which is what `ABS` of a signed column changes.
                "min" | "max" | "abs" => match folded_parts() {
                    Some((precision, _, scale)) => sized(precision, scale),
                    None => scale_only_or_default(folded_shape.and_then(|shape| shape.scale)),
                },
                "round" | "truncate" => {
                    rounded_shape(&name, &args, refs, input, sub, signed_here())
                }
                "coalesce" | "ifnull" | "nvl" | "greatest" | "least" | "nullif" => {
                    widest_candidate(&args, refs, input, sub, signed_here())
                }
                // `IF` chooses between its second and third arguments; the
                // condition contributes nothing to the shape.
                "if" if args.len() == 3 => {
                    widest_candidate(&args[1..], refs, input, sub, signed_here())
                }
                _ => Exact::default(),
            }
        }
        _ => Exact::default(),
    }
}

/// A shape with places but no width, or nothing at all.
fn scale_only_or_default(scale: Option<u32>) -> Exact {
    match scale {
        Some(scale) => Exact::scale_only(scale),
        None => Exact::default(),
    }
}

/// The shape a family that picks one of several candidates takes: the widest
/// integral part and the widest scale, with no room added for a carry.
///
/// Measured on 8.3 over a `DECIMAL(10,2)` and a `DECIMAL(7,3)`: `GREATEST`,
/// `LEAST`, `COALESCE`, `IFNULL`, `IF` and `CASE` all report `len 13 dec 3`,
/// where the addition of the same two is 14.
fn widest_candidate(
    candidates: &[Expr],
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
    signed: bool,
) -> Exact {
    // Before the fold rather than after it: over no candidates at all the
    // fold answers a shape of nothing, which would describe the column as a
    // `decimal(0,0)` rather than leave it alone.
    if candidates.is_empty() {
        return Exact::default();
    }
    let mut integral = 0;
    let mut scale = 0;
    for candidate in candidates {
        let shape = exact_shape(candidate, refs, input, sub);
        let unsigned = unsigned_answer(candidate, refs, input, sub);
        let (Some(candidate_integral), Some(candidate_scale)) =
            (exact_integral(shape, unsigned), shape.scale)
        else {
            return Exact::default();
        };
        integral = integral.max(candidate_integral);
        scale = widest_scale(scale, candidate_scale);
    }
    Exact::new(
        exact_length((integral + scale).min(MAX_PRECISION), scale, !signed),
        scale,
    )
}

/// The shape `ROUND` and `TRUNCATE` take, which differ by one digit.
///
/// Both cut the scale to what was asked for, and neither can widen it past the
/// argument's: `ROUND(a, 5)` over a `DECIMAL(10,2)` is still `len 12 dec 2` on
/// 8.3. What they do with the digits they gave up is where they part company —
/// `ROUND` hands one back, because rounding up can carry into a new one, and
/// `TRUNCATE` does not. Measured: `ROUND(a, 1)` is 12 wide and `TRUNCATE(a, 1)`
/// is 11.
///
/// A negative scale rounds to a whole number, and a scale that is not a
/// literal is not known here at all — `ROUND(a, k)` over a column keeps the
/// argument's shape, which is what `None` from `literal_scale` falls to.
fn rounded_shape(
    name: &str,
    args: &[Expr],
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
    signed: bool,
) -> Exact {
    let Some(subject) = args.first() else {
        return Exact::default();
    };
    let shape = exact_shape(subject, refs, input, sub);
    let unsigned = unsigned_answer(subject, refs, input, sub);
    let (Some(precision), Some(scale)) = (exact_precision(shape, unsigned), shape.scale) else {
        return Exact::default();
    };
    // One argument means "to a whole number", which is `ROUND(a)`.
    let asked = match args.get(1) {
        None => Some(0),
        Some(second) => literal_scale(second),
    };
    let Some(asked) = asked else {
        return Exact::new(exact_length(precision, scale, !signed), scale);
    };
    let kept = asked.clamp(0, i64::from(scale)) as u32;
    // Neither subtraction below can go under zero: `kept` is clamped to
    // `scale`, and `exact_precision` hands back a precision only where it is
    // at least the scale.
    let given_up = scale - kept;
    let width = if name == "round" {
        // MySQL's `length_increase`: a carry needs a digit, and it needs one
        // per nine-digit word given up, which is why the test is a remainder.
        precision - given_up + u32::from(given_up % 9 > 0)
    } else {
        precision - scale + kept
    };
    // A number has at least one digit. Giving up every place of a
    // `DECIMAL(9,9)` leaves none by the arithmetic above — nine is a whole
    // word, so nothing is handed back for the carry — and 8.3 declares
    // `decimal(1,0)` there rather than a precision of nothing, for both
    // spellings. Measured; `DECIMAL(4,4)` is the case that separates them,
    // where `ROUND` reaches 1 on its own and `TRUNCATE` does not.
    Exact::new(exact_length(width.max(1), kept, !signed), kept)
}

/// The scale a `ROUND` or `TRUNCATE` was written with, where it was written as
/// a number.
fn literal_scale(e: &Expr) -> Option<i64> {
    match e {
        Expr::Nested(inner) => literal_scale(inner),
        Expr::UnaryOp {
            op: sqlparser::ast::UnaryOperator::Minus,
            expr,
        } => Some(-literal_scale(expr)?),
        Expr::Value(v) => match &v.value {
            sqlparser::ast::Value::Number(text, _) => text.parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// How many characters an integer expression's widest value can print.
///
/// MySQL's `Item::max_length` for the integer family, and it decides two
/// things: the width the column goes out with, and — where it is nine or fewer
/// — that a materialised copy of the column is an `int` rather than a
/// `bigint`. See [`OutColumn::materialised_sql_type`].
///
/// Every number below is measured on 8.3 by selecting the expression and
/// reading `Length` off the column definition:
///
/// | expression | width |
/// |---|---|
/// | `1`, `-1` | 2 |
/// | `127` | 4 |
/// | `9223372036854775807` | 20 |
/// | `1 + 1` | 3 |
/// | `ti + 1` over a `TINYINT` | 5 |
/// | `ti * si` over a `TINYINT` and a `SMALLINT` | 9 |
/// | `1 MOD 3`, `7 DIV 2`, `ABS(-5)` | 2 |
/// | `1 = 1`, `1 IS NULL`, `NOT 1`, `1 IN (1,2)` | 1 |
///
/// So an integer literal is its digits plus a column for the sign, an addition
/// is the wider operand plus one for the carry, a multiplication is the two
/// operands' digits plus one, `DIV` cannot exceed its dividend, `MOD` cannot
/// exceed either operand, and everything that answers yes or no is one
/// character.
///
/// `None` is "not modelled", and it reads as *wide* — which is deliberate and
/// is why this can be a subset rather than a whole. MySQL's own default for an
/// integer function is 21, so `SIGN`, `CEIL`, `COUNT(*)`, `CAST(x AS SIGNED)`
/// and every bitwise operator are `bigint` there too, and answering `None` for
/// them is the right answer by the shortest route. The functions that are
/// *not* — `MONTH` is 3, `LENGTH` is 10, `ASCII` is 3 — are a width per
/// function, and are #0188: a missing width keeps the answer this engine
/// already gave, where a wrong one would invent a new divergence.
///
/// The *root* is known to be an integer, because [`item_shape`] checked it; a
/// subexpression is not, and does not have to be. A width that comes back from
/// one can only reach a caller through an operator, and an operator over
/// something that is not an integer does not answer one either — so the column
/// is not a `bigint` and `materialised_sql_type` leaves it alone whatever
/// number arrives here. Nothing below re-asks the type for that reason:
/// `computed_column_type` is not free.
fn printed_width(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
) -> Option<u32> {
    use sqlparser::ast::BinaryOperator as B;
    use sqlparser::ast::UnaryOperator as U;

    /// One character of yes or no, for everything that answers a predicate.
    const BOOLEAN: u32 = 1;

    let recur = |inner: &Expr| printed_width(inner, refs, input);
    // The corrected shape, for the reason `computed_column_type` asks for it
    // just above: the operator at the root of the tree as parsed is often not
    // the operator that decides, and the walkers over these trees have to read
    // the same shape `eval` does or they describe a different expression than
    // the one that runs.
    if let Some(fixed) = crate::expr::reassociated_postfix(e) {
        return printed_width(&fixed, refs, input);
    }
    match e {
        Expr::Nested(inner) => recur(inner),
        // A column reference is the display width it was declared with — 4 for
        // a `TINYINT`, 11 for an `INT`, 4 for a `YEAR` — which is what
        // `wire_length` already answers, and the same number MySQL reports for
        // the bare column.
        //
        // Only for a column that is a number to begin with. A temporal
        // computes as one and is not sized like one -- it computes in the
        // digits of its packed form and prints in rather more -- so its
        // display width is the wrong number to hand to the arithmetic below.
        // Measured on 8.3: `tm + 0` over a `TIME` is an `int`, where the ten
        // characters `-838:59:59` takes would ask for a `bigint`, while
        // `dd + 0` over a `DATE` really is a `bigint`. Every temporal this
        // engine has happens to fall on the `bigint` side either way today, so
        // this changes no answer -- it keeps a number that is wrong for the
        // right reason out of a sum that would then be wrong for no reason.
        // The widths themselves are a measurement per temporal type and are
        // left unmade rather than guessed at.
        //
        // Reachable only from inside a larger expression: a reference on its
        // own keeps its declaration, and a materialisation copies that field
        // rather than deriving one.
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => direct_column(e, refs, input)
            .filter(|column| {
                matches!(
                    column.ty.kind,
                    crate::types::TypeKind::Int
                        | crate::types::TypeKind::Bool
                        | crate::types::TypeKind::Year
                )
            })
            .map(|column| column.ty.wire_length()),
        Expr::Value(v) => match &v.value {
            // The digits, plus one for the sign: `1` is 2 on 8.3 and `127` is
            // 4. A point or an exponent is not an integer and has no width of
            // this shape, so the digits are checked rather than assumed — a
            // subexpression arrives here unguarded, and `10.5` would otherwise
            // count its point as a digit.
            //
            // `-1` is one token to MySQL's parser and two to this one; the
            // unary arm below hands back the same 2 either way, and the prefix
            // is stripped here in case a dialect ever folds it in.
            sqlparser::ast::Value::Number(text, _) => {
                let digits = text.strip_prefix('-').unwrap_or(text);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                u32::try_from(digits.len())
                    .ok()
                    .map(|digits| digits.saturating_add(1))
            }
            // `TRUE` and `FALSE` are the constants 1 and 0.
            sqlparser::ast::Value::Boolean(_) => Some(BOOLEAN),
            _ => None,
        },
        Expr::UnaryOp { op, expr } => {
            if let Some(fixed) = crate::expr::reassociated_unary(op, expr) {
                return printed_width(&fixed, refs, input);
            }
            match op {
                // Neither sign adds a character: the width already carries a
                // column for one. `-(1)` is 2 on 8.3, as `1` is.
                U::Plus | U::Minus => recur(expr),
                U::Not => Some(BOOLEAN),
                _ => None,
            }
        }
        Expr::BinaryOp { left, op, right } => {
            if let Some(fixed) = crate::expr::reassociated(op, left, right) {
                return printed_width(&fixed, refs, input);
            }
            match op {
                // One more than the wider operand, for the carry.
                B::Plus | B::Minus => Some(recur(left)?.max(recur(right)?).saturating_add(1)),
                // The digits of both, plus a column for the sign — so the two
                // widths added, less the one sign column they each already
                // carry. `ti * si` is 4 + 6 - 1 = 9 on 8.3, and `i * i`
                // 11 + 11 - 1 = 21.
                B::Multiply => Some(recur(left)?.saturating_add(recur(right)?).saturating_sub(1)),
                // A quotient cannot be wider than what was divided, and a
                // remainder cannot be wider than either side.
                B::MyIntegerDivide => recur(left),
                B::Modulo => Some(recur(left)?.max(recur(right)?)),
                B::Eq
                | B::NotEq
                | B::Lt
                | B::LtEq
                | B::Gt
                | B::GtEq
                | B::Spaceship
                | B::And
                | B::Or
                | B::Xor => Some(BOOLEAN),
                // The other reading of `||`, which is the connective unless
                // the mode says otherwise. `computed_column_type` splits the
                // token the same way.
                B::StringConcat if !pipes_as_concat() => Some(BOOLEAN),
                _ => None,
            }
        }
        Expr::IsNull(_)
        | Expr::IsNotNull(_)
        | Expr::IsTrue(_)
        | Expr::IsFalse(_)
        | Expr::IsNotTrue(_)
        | Expr::IsNotFalse(_)
        | Expr::InList { .. }
        | Expr::InSubquery { .. }
        | Expr::Between { .. }
        | Expr::Exists { .. }
        | Expr::Like { .. }
        | Expr::ILike { .. }
        | Expr::RLike { .. }
        | Expr::IsDistinctFrom(_, _)
        | Expr::IsNotDistinctFrom(_, _) => Some(BOOLEAN),
        // `ABS` cannot widen what it was given: the negative half of an
        // integer is the deeper one, so the sign column it gives up is the
        // digit the positive half would need. 8.3 reports 2 for `ABS(-5)` and
        // 11 for `ABS(-2147483648)`, which are the widths of the literals.
        Expr::Function(f) if f.name.to_string().eq_ignore_ascii_case("abs") => {
            match function_arguments(f).as_slice() {
                [argument] => recur(argument),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether MySQL puts this query block in a temporary table rather than
/// merging it into the query that reads it.
///
/// It decides whether a column materialised out of the block is narrowed by
/// [`OutColumn::materialised_sql_type`], and it exists because this engine has
/// no merge: every derived table, CTE and view is run and its rows fed back in
/// as a relation, so without this the narrowing would be applied to blocks
/// MySQL never materialises at all. Measured on 8.3 — `SELECT * FROM (SELECT
/// ti + 1 AS a FROM Ints) z` is a LONGLONG carrying the BINARY flag, the
/// expression itself, where the same derived table without the FROM clause is
/// a LONG without it.
///
/// A FROM-less block is the case that cannot be merged, and it is the whole of
/// what this claims. MySQL also materialises for DISTINCT, GROUP BY, LIMIT and
/// a window function — measured, all three of `SELECT * FROM (SELECT ti + 1 AS
/// a FROM Ints LIMIT 1) z`, `... DISTINCT ...` and `... GROUP BY 1` are LONG —
/// and this does not say so, because each of those blocks reads its width from
/// a *column* and the widths of columns through those clauses are not what
/// #0110 measured. Saying less leaves the answer this engine already gave.
///
/// A set operation is not one of them, which is measured rather than assumed:
/// `SELECT 1 MOD 3 UNION SELECT 4` materialises — it loses the BINARY flag —
/// and stays a LONGLONG, because a UNION's column is typed by aggregating the
/// branches' *types* and never their widths.
fn definition_materialised_by_mysql(session: &Session, definition: &str) -> bool {
    // A view is stored as the text it was written with, so the block has to be
    // parsed back to be asked. Through `normalise`, because that text is
    // MySQL's and not sqlparser's — `1 MOD 3` is the case #0110 is about, and
    // it is the rewriter that turns the `MOD` into the `%` this parser reads.
    //
    // A definition that will not parse is left unnarrowed rather than guessed
    // at; running the view is what reports the problem.
    let normalised = rewrite::normalise(
        rewrite::Normalised::untouched(definition),
        session.sql_mode(),
    );
    let Ok(statements) = Parser::parse_sql(&MySqlDialect {}, &normalised.sql) else {
        return false;
    };
    matches!(statements.as_slice(), [Statement::Query(query)] if materialised_by_mysql(query))
}

fn materialised_by_mysql(query: &Query) -> bool {
    match query.body.as_ref() {
        SetExpr::Select(select) => select.from.is_empty(),
        SetExpr::Query(inner) => materialised_by_mysql(inner),
        _ => false,
    }
}

/// The input column an expression names outright, if it names one.
///
/// A bare reference carries the type it was declared with; anything computed
/// from it does not. Everywhere that difference matters asks here rather than
/// re-deriving it, so the projection and the grouped path cannot disagree
/// about whether `id` is an `INT` or a widened `BIGINT`.
fn direct_column<'a>(
    e: &Expr,
    refs: &[ColumnRef],
    input: &'a [crate::relation::RelColumn],
) -> Option<&'a crate::relation::RelColumn> {
    let written = column_reference(e)?;
    column_slot(refs, written).and_then(|slot| input.get(slot))
}

/// What a scalar subquery's column is described against.
///
/// The type of `(SELECT ...)` is the type of the subquery's *own* projection,
/// so nothing about the enclosing statement can answer it: the inner query has
/// to be described, which needs the catalog, the session and therefore the
/// engine. Every walker below that can meet a subquery carries this, and
/// `None` means there is nothing to describe with — a column default, a check
/// constraint, a unit test — where the answer stays what it was before this
/// existed.
#[derive(Clone, Copy)]
struct Sub<'a> {
    engine: &'a Engine,
    session: &'a Session,
}

thread_local! {
    /// The scalar subqueries already described while typing this statement,
    /// by the scope they were described in and the text they were written as.
    ///
    /// Four walkers ask about the same subquery node — its type, its sign, its
    /// fractional precision and whether it can be NULL — and describing it is
    /// a whole query. Without this each of them ran it again, and a subquery
    /// inside a subquery multiplied that per level.
    ///
    /// Keyed by the scope as well as the text because a correlated subquery's
    /// answer depends on what encloses it, and one statement can write the
    /// same text under two of them.
    static SUBQUERY_COLUMNS: std::cell::RefCell<HashMap<String, Option<OutColumn>>> =
        std::cell::RefCell::new(HashMap::new());

    /// How many scalar subqueries deep the current description is.
    static SUBQUERY_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How far a scalar subquery is described into a nest of them.
///
/// Describing one runs it, and running it types *its* projection, which
/// describes the subqueries in that. A statement can nest them as deeply as it
/// likes, so without a stop the describe recurses as deep as the text does and
/// runs out of stack rather than answering. Past the limit the type falls back
/// to `VAR_STRING`, which is what every one of these was before this change —
/// a worse answer, not a wrong one.
const SUBQUERY_DEPTH_LIMIT: usize = 8;

/// Restores the depth however the scope is left, as `Shaping` does.
struct SubqueryDepth;

impl Drop for SubqueryDepth {
    fn drop(&mut self) {
        SUBQUERY_DEPTH.with(|d| d.set(d.get() - 1));
    }
}

/// Clears the memo above for the duration of one statement.
///
/// Entered at `execute` and `describe`, which are the only ways in. The
/// previous map is put back rather than dropped because those two can nest —
/// `ALTER TABLE` re-resolves the views over the table it changed — and a
/// statement that cleared an enclosing one's memo would leave it describing
/// nothing while still believing it had.
struct SubqueryMemo(HashMap<String, Option<OutColumn>>);

impl SubqueryMemo {
    fn fresh() -> Self {
        Self(SUBQUERY_COLUMNS.with(|m| std::mem::take(&mut *m.borrow_mut())))
    }
}

impl Drop for SubqueryMemo {
    fn drop(&mut self) {
        SUBQUERY_COLUMNS.with(|m| *m.borrow_mut() = std::mem::take(&mut self.0));
    }
}

/// The one column a scalar subquery produces, described rather than evaluated.
///
/// Run as a probe over no rows, for the two reasons `describe` is: a type is
/// not a value, so the rows are wasted work — and a `SELECT` is not free of
/// consequences, so typing `(SELECT GET_LOCK('x', 0))` must not take the lock
/// a second time. That also keeps this answering the same thing at prepare and
/// at execute, which is the invariant `describe_matches_execute` holds.
///
/// `None` where there is nothing to describe with, where the subquery does not
/// resolve — a correlated reference this scope does not carry — or where it
/// produces anything other than exactly one column. The last of those is 1241
/// on 8.3 and answered here rather than refused, because this is a walker that
/// types an expression and has no way to report an error; the refusal is its
/// own issue.
fn subquery_column(sub: Option<Sub<'_>>, query: &Query, refs: &[ColumnRef]) -> Option<OutColumn> {
    let sub = sub?;
    let depth = SUBQUERY_DEPTH.with(|d| d.get());
    if depth >= SUBQUERY_DEPTH_LIMIT {
        return None;
    }

    // The depth is part of the key because it is part of the answer: the limit
    // above returns without storing anything, but every level *over* the one
    // that hit it stores a type derived from the fallback. Sharing that with a
    // shallower lookup of the same query would hand it a truncated answer it
    // had the budget to compute properly.
    let mut key = format!("{depth}:");
    for r in refs {
        key.push_str(&r.database);
        key.push('.');
        key.push_str(&r.table);
        key.push('.');
        key.push_str(&r.name);
        key.push(',');
    }
    key.push_str("::");
    key.push_str(&query.to_string());
    if let Some(hit) = SUBQUERY_COLUMNS.with(|m| m.borrow().get(&key).cloned()) {
        return hit;
    }

    // The enclosing columns with no values in them, exactly as `describe`
    // binds every parameter to NULL: a correlated subquery has to *resolve*
    // against the outer row, and nothing about which row it is changes the
    // type it answers with.
    let outer = OuterRow {
        schema: refs.to_vec(),
        values: vec![Value::Null; refs.len()],
    };

    SUBQUERY_DEPTH.with(|d| d.set(d.get() + 1));
    let _depth = SubqueryDepth;
    let described = expr::probing(|| {
        shaping(|| {
            sub.engine
                .query_outer(sub.session, query, Some(&outer))
                .ok()
        })
    });
    let answer = match described {
        Some(result) if result.columns.len() == 1 => result.columns.into_iter().next(),
        _ => None,
    };
    SUBQUERY_COLUMNS.with(|m| m.borrow_mut().insert(key, answer.clone()));
    answer
}

/// Whether a query answers with a row whatever is in the database.
///
/// The other half of a scalar subquery's nullability: 8.3 keeps the NOT_NULL
/// flag on `(SELECT 1)` and takes it off `(SELECT sid FROM T)` even though
/// `sid` is declared NOT NULL, because the table can be empty and an empty
/// subquery is NULL. So the flag survives only where the query reads no table
/// and filters nothing.
///
/// A LIMIT takes the flag off here where 8.3 keeps it, and that is deliberate:
/// `SELECT (SELECT 1 LIMIT 0)` is NOT_NULL *and answers 1* on 8.3, which never
/// applies the clause to a constant subquery at all. This engine does apply it
/// and answers NULL, so following MySQL's flag would put NOT_NULL over a NULL
/// — and under-reporting is the safe direction, as `never_null` says. It costs
/// `(SELECT 1 LIMIT 1)`, which 8.3 calls NOT_NULL.
///
/// A WHERE and a HAVING are read, and there MySQL agrees: `(SELECT 1 WHERE 0)`
/// and `(SELECT 1 HAVING 1)` are both nullable on 8.3.
fn always_one_row(query: &Query) -> bool {
    query.with.is_none() && query.limit_clause.is_none() && body_always_one_row(query.body.as_ref())
}

/// [`always_one_row`] for a query's body, which is what a set operation's arms
/// are.
fn body_always_one_row(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(select) => {
            select.from.is_empty()
                && select.selection.is_none()
                && select.having.is_none()
                && matches!(&select.group_by, GroupByExpr::Expressions(e, _) if e.is_empty())
        }
        // A UNION answers whatever its branches answer, so if both always
        // have a row so does it: 8.3 reports `(SELECT 1 UNION SELECT 1)`
        // NOT_NULL. The other three take rows *away* — `SELECT 1 EXCEPT
        // SELECT 1` and `SELECT 1 INTERSECT SELECT 2` are both empty however
        // certain their branches are — and 8.3 reports both of those
        // nullable. Reading the operator is what tells them apart; without it
        // this engine put NOT_NULL over the NULL it really sends for them.
        SetExpr::SetOperation {
            op: sqlparser::ast::SetOperator::Union,
            left,
            right,
            ..
        } => body_always_one_row(left) && body_always_one_row(right),
        SetExpr::Query(inner) => always_one_row(inner),
        _ => false,
    }
}

/// The type to declare for an expression, before any row has been evaluated.
///
/// A grouped projection cannot be typed from the first group's value: a later
/// group may produce a different one, and the binary protocol encodes by the
/// declared type, so the client would decode the wrong thing.
///
/// `refs` carries the input columns so an aggregate over one can keep its type:
/// `SUM(views)` of an integer column is an integer, and reporting it as a
/// string makes a typed client hand back `"120"` where MySQL hands back `120`.
fn computed_column_type(
    e: &Expr,
    refs: &[ColumnRef],
    input: &[crate::relation::RelColumn],
    sub: Option<Sub<'_>>,
) -> ColumnType {
    // The corrected shape, for the reason the `BinaryOp` arm below asks for
    // it: the root as parsed is often not the operator that decides. Every
    // construct here and every operator that can be sunk past one is a
    // LONGLONG, so this changes no answer today — and MySQL's grammar is what
    // holds that, not luck: see the note in `unsigned_answer`. The walkers
    // over these trees have to agree with the shape `eval` reads, and
    // `never_null` already does.
    // Through the same door `eval` uses, which also carries the `BINARY`
    // prefix back to the operand it belongs to. That one *does* change the
    // answer here — `SELECT BINARY 'A' LIKE 'a'` is a LONGLONG on 8.3 and was
    // a VAR_STRING — and describing a column one way while evaluating it
    // another is how a value gets rendered as the type it is not.
    if let Some(fixed) = crate::expr::corrected_root(e) {
        return computed_column_type(&fixed, refs, input, sub);
    }
    match e {
        Expr::Function(f) => {
            let name = f.name.to_string().to_ascii_lowercase();
            match name.as_str() {
                // MIN and MAX keep the type of what they fold, because the
                // smallest and largest of a column are values of that column.
                "min" | "max" => aggregate_argument(f)
                    .map(|argument| computed_column_type(&argument, refs, input, sub))
                    .unwrap_or(ColumnType::VarString),
                // A total is not. `SUM` over a TINYINT reaches 300, and
                // announcing the column as one byte makes the binary protocol
                // send one byte — the client reads 44. MySQL answers
                // NEWDECIMAL for every exact type for exactly this reason,
                // and DOUBLE where the argument was already approximate.
                "sum" | "avg" => {
                    let argument = aggregate_argument(f)
                        .map(|argument| computed_column_type(&argument, refs, input, sub));
                    match argument {
                        Some(ColumnType::Float | ColumnType::Double) => ColumnType::Double,
                        // A string has no exact type to total up, so the
                        // total is approximate. Which codes count as one, and
                        // why `VAR_STRING` is not among them, is on
                        // `is_textual_code`.
                        Some(t) if is_textual_code(t) => ColumnType::Double,
                        _ => ColumnType::NewDecimal,
                    }
                }
                // A window over an integer widens to `LONGLONG`, as MySQL
                // does: the frame can carry a value the column's width cannot.
                "lag" | "lead" | "first_value" | "last_value" | "nth_value" => {
                    let argument = function_arguments(f)
                        .first()
                        .map(|argument| computed_column_type(argument, refs, input, sub));
                    match argument {
                        Some(t) if is_integer(t) => ColumnType::LongLong,
                        Some(t) => t,
                        None => ColumnType::VarString,
                    }
                }
                // Shifting a date by an interval gives back what it was given
                // — including when what it was given was a string, which is
                // how it is usually written: `DATE_ADD('2026-01-01', ...)` is
                // a `STRING` to MySQL and only a `DATETIME` when the argument
                // was one.
                "date_add" | "date_sub" | "adddate" | "subdate" | "timestampadd" => {
                    // `TIMESTAMPADD(HOUR, 1, dt)` puts the unit first, so the
                    // value it shifts is the third argument. Reading the first
                    // resolved `HOUR` as an unknown column and answered with a
                    // string.
                    let args = function_arguments(f);
                    let subject = if name == "timestampadd" {
                        args.get(2)
                    } else {
                        args.first()
                    };
                    match subject.map(|argument| computed_column_type(argument, refs, input, sub)) {
                        Some(ColumnType::Date) => ColumnType::Date,
                        Some(ColumnType::DateTime | ColumnType::Timestamp) => ColumnType::DateTime,
                        // MySQL answers `STRING` here rather than
                        // `VAR_STRING` — a fixed-width string, which is the
                        // one place a computed column gets that code.
                        _ => ColumnType::String,
                    }
                }
                // These hand back one of their arguments, so they are typed by
                // them — widened to `LONGLONG` for integers, as MySQL does.
                // `NULLIF` is the exception and keeps the argument's own width.
                "coalesce" | "ifnull" | "nvl" | "if" => {
                    let args = function_arguments(f);
                    // `IF` chooses between its second and third; the condition
                    // says nothing about the result. `get(..)` rather than a
                    // slice, because `SELECT IF()` is a statement anyone can
                    // send and indexing an empty argument list aborted the
                    // whole process.
                    let candidates = if name == "if" {
                        args.get(1..).unwrap_or(&[])
                    } else {
                        &args[..]
                    };
                    // Every candidate, not the first: the others are what gets
                    // returned when the first is NULL. `COALESCE(i, 'x')` is a
                    // string in MySQL, and calling it a LONGLONG had the binary
                    // protocol send 0 for the `'x'`.
                    combined(
                        candidates
                            .iter()
                            .map(|a| candidate_type(a, refs, input, sub)),
                    )
                }
                // These hand back one of their arguments unchanged, so they
                // keep its width: `GREATEST(tinyint, smallint)` really is a
                // SMALLINT to MySQL, and a string argument makes the answer a
                // string. Declaring them LONGLONG had the binary protocol
                // send 0 for the string.
                "greatest" | "least" => combined(
                    function_arguments(f)
                        .iter()
                        .map(|a| candidate_type(a, refs, input, sub)),
                ),
                // These keep the *kind* of what they were given but not its
                // width — MySQL answers LONGLONG for every integer argument,
                // however narrow, and a decimal or a double for those.
                // Fixing them at LONGLONG truncated a decimal to its integer
                // part; typing them by the argument sent `ABS(tinyint)` as one
                // byte, which is a value the answer can outgrow.
                //
                // The *first* argument, because the second of `ROUND(x, 2)`
                // says how many places to keep and is not a value being
                // rounded: reading both made `ROUND(NULL, 1)` a LONGLONG on
                // the strength of the 1, where MySQL says DOUBLE. It ignores
                // that argument entirely — `ROUND(1, NULL)` is a LONGLONG
                // there.
                "abs" | "round" | "truncate" => numeric_answer(
                    function_arguments(f)
                        .first()
                        .map(|a| computed_column_type(a, refs, input, sub))
                        .unwrap_or(ColumnType::Null),
                    false,
                ),
                // `MOD` is the `%` operator spelled as a call, and MySQL types
                // the two identically — including the DOUBLE it answers when
                // either side has no numeric kind of its own.
                "mod" => {
                    let args = function_arguments(f);
                    let side = |i: usize| {
                        args.get(i)
                            .map(|a| arithmetic_operand_type(a, refs, input, sub))
                            .unwrap_or(ColumnType::Null)
                    };
                    arithmetic_type(side(0), side(1))
                }
                // The same, except that the answer is whole, so an exact
                // argument gives an integer rather than a decimal.
                "ceiling" => numeric_answer(
                    combined(
                        function_arguments(f)
                            .iter()
                            .map(|a| candidate_type(a, refs, input, sub)),
                    ),
                    true,
                ),
                // `NULLIF` answers its first argument or NULL, and both fit
                // that argument's own width.
                "nullif" => function_arguments(f)
                    .first()
                    .map(|argument| computed_column_type(argument, refs, input, sub))
                    .unwrap_or(ColumnType::VarString),
                "isnull" => ColumnType::LongLong,
                // A string function MySQL sizes from its arguments takes its
                // *code* from that width too, so the same call is a
                // VAR_STRING over a `CHAR(10)` and a MEDIUM_BLOB over a
                // `TEXT`. #0142.
                other if widens_with_argument(other) => widened_string_type(e, refs, input, sub),
                // The ones whose answer does not depend on what they were
                // given, and then everything unrecognised, which stays a
                // string because nothing here knows better.
                other => function_result_type(other).unwrap_or(ColumnType::VarString),
            }
        }
        // A column reference keeps the type it was declared with.
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => direct_column(e, refs, input)
            .map(|column| column.ty.column_type())
            .unwrap_or(ColumnType::VarString),
        // A literal is typed by what it is. A number with a point is a
        // DECIMAL to MySQL rather than a double, and NULL has its own code.
        Expr::Value(v) => match &v.value {
            sqlparser::ast::Value::Number(text, _) => {
                // A point makes a DECIMAL and an exponent makes a DOUBLE —
                // `1.5` is NEWDECIMAL to MySQL and `1e3` is DOUBLE.
                if text.contains('e') || text.contains('E') {
                    ColumnType::Double
                } else if text.contains('.') {
                    ColumnType::NewDecimal
                } else if text.parse::<i64>().is_ok() || text.parse::<u64>().is_ok() {
                    ColumnType::LongLong
                } else {
                    // Past both integers and still exact: MySQL types
                    // `18446744073709551616` NEWDECIMAL and prints all twenty
                    // digits. Announced LONGLONG, the binary protocol would
                    // have sent eight bytes for a number that does not fit in
                    // them.
                    ColumnType::NewDecimal
                }
            }
            sqlparser::ast::Value::Boolean(_) => ColumnType::LongLong,
            sqlparser::ast::Value::Null => ColumnType::Null,
            _ => ColumnType::VarString,
        },
        // The expressions `sqlparser` gives their own grammar rather than
        // routing through `Expr::Function`, so `function_result_type` never
        // sees them. `POSITION` and `EXTRACT` answered `VAR_STRING` here until
        // this arm existed, where MySQL says `LONGLONG` both times.
        Expr::Position { .. } | Expr::Extract { .. } => ColumnType::LongLong,
        // `CEIL` and `FLOOR` are typed like the `CEILING` spelling they are
        // synonyms of, which is by their argument: whole, but a DOUBLE when
        // what it rounded was one.
        Expr::Floor { expr, .. } | Expr::Ceil { expr, .. } => {
            numeric_answer(computed_column_type(expr, refs, input, sub), true)
        }
        // `SUBSTRING` and `TRIM` are sized from their argument as the calls
        // above are; `OVERLAY` is not modelled and keeps the old answer.
        Expr::Substring { .. } | Expr::Trim { .. } => widened_string_type(e, refs, input, sub),
        Expr::Overlay { .. } => ColumnType::VarString,
        // Anything that answers a yes-or-no question is an integer, which is
        // the largest single group MySQL types differently from this engine.
        Expr::IsNull(_)
        | Expr::IsNotNull(_)
        | Expr::IsTrue(_)
        | Expr::IsFalse(_)
        | Expr::IsNotTrue(_)
        | Expr::IsNotFalse(_)
        | Expr::InList { .. }
        | Expr::InSubquery { .. }
        | Expr::Between { .. }
        | Expr::Exists { .. }
        | Expr::Like { .. }
        | Expr::ILike { .. }
        | Expr::RLike { .. }
        | Expr::IsDistinctFrom(_, _)
        | Expr::IsNotDistinctFrom(_, _) => ColumnType::LongLong,
        Expr::BinaryOp { left, op, right } => {
            use sqlparser::ast::BinaryOperator as B;
            // The corrected shape usually has some other operator at its
            // root, and that root is what decides the type: `a DIV b * 1.5` is
            // a decimal, and `doc->'$.b' = 6` is a boolean rather than a
            // document.
            if let Some(fixed) = crate::expr::reassociated(op, left, right) {
                return computed_column_type(&fixed, refs, input, sub);
            }
            match op {
                // A comparison or a logical connective is 0 or 1. `<=>` is
                // one of them and had been left out, so it was described
                // VAR_STRING while answering an integer — found by putting it
                // in the corpus for #0061, where every other comparison was
                // already right.
                B::Eq
                | B::NotEq
                | B::Lt
                | B::LtEq
                | B::Gt
                | B::GtEq
                | B::Spaceship
                | B::And
                | B::Or
                | B::Xor => ColumnType::LongLong,
                // `||` is two operators wearing one token, and only one of
                // them concatenates. Under the default mode it is OR and
                // MySQL describes it as it describes every other connective;
                // reading the token as a concatenation whatever the mode said
                // announced `1 || 1` VAR_STRING while answering 1.
                B::StringConcat if !pipes_as_concat() => ColumnType::LongLong,
                // Division is exact in MySQL, so it answers a decimal even
                // over two integers — where `DIV` is integer division and is a
                // LONGLONG whatever it divided, including two decimals.
                //
                // Approximate on either side makes it a DOUBLE, like every
                // other operator: measured on 8.3, `d / f` over a DECIMAL and
                // a FLOAT is DOUBLE and `d / i` is NEWDECIMAL.
                //
                // Asked of the operands directly rather than read off
                // `arithmetic_type`, whose DOUBLE also means "I could not type
                // this" — an unrecognised function, and a scalar subquery
                // until #0104 gave it its column's type. Routed through
                // it, `(SELECT 1)/2` was announced DOUBLE and `reconcile`
                // then parsed the exact quotient back through an `f64`, so the
                // same expression answered 0.5 as a column and 0.5000 inside a
                // `CONCAT`. An operand this cannot type is *not* known to be
                // approximate, and a decimal is the answer MySQL gives when
                // neither side is.
                B::Divide => {
                    let approximate =
                        |e| is_approximate_code(arithmetic_operand_type(e, refs, input, sub));
                    if approximate(left) || approximate(right) {
                        ColumnType::Double
                    } else {
                        ColumnType::NewDecimal
                    }
                }
                B::MyIntegerDivide => ColumnType::LongLong,
                B::Plus | B::Minus | B::Multiply | B::Modulo => arithmetic_type(
                    arithmetic_operand_type(left, refs, input, sub),
                    arithmetic_operand_type(right, refs, input, sub),
                ),
                // Every bitwise operator answers 64 unsigned bits whatever it
                // was given: `1.5 ^ 1` is a LONGLONG in MySQL and so is
                // `'3' & 1`. The UNSIGNED flag is `unsigned_answer`'s.
                B::BitwiseAnd
                | B::BitwiseOr
                | B::BitwiseXor
                | B::PGBitwiseShiftLeft
                | B::PGBitwiseShiftRight => ColumnType::LongLong,
                // `->` yields a document and `->>` the text inside one.
                B::Arrow => ColumnType::Json,
                B::LongArrow => ColumnType::LongBlob,
                _ => ColumnType::VarString,
            }
        }
        Expr::UnaryOp { op, expr } => {
            use sqlparser::ast::UnaryOperator as U;
            // As with the binary operators above, the corrected shape decides:
            // `~1 * 2` arrives as `~(1 * 2)` and is really a multiplication.
            if let Some(fixed) = crate::expr::reassociated_unary(op, expr) {
                return computed_column_type(&fixed, refs, input, sub);
            }
            match op {
                U::Not | U::BitwiseNot => ColumnType::LongLong,
                // Unary plus is not an operator at all — MySQL's parser hands
                // its operand straight back — so the operand's type is the
                // answer, untouched. It shared the arm below for as long as
                // both were called signs, which gave a plus every coercion a
                // minus makes: measured on 8.3, `NULLIF(+f, 1)` over a FLOAT
                // is a FLOAT where `NULLIF(-f, 1)` is a DOUBLE, and
                // `NULLIF(+b, 1)` over a TINYINT is a TINY where the minus
                // widens to LONGLONG. Only reachable from inside another
                // expression, since a plus over a column reference on its own
                // is that column and never comes here.
                U::Plus => computed_column_type(expr, refs, input, sub),
                // Unary minus computes, and what it computes is always a
                // number: MySQL has no spelling of `-x` that answers a string.
                // So the three arms below are the whole of it — an integer of
                // any width negates to a LONGLONG, an exact fraction stays
                // exact, and everything else is a DOUBLE.
                //
                // The catch-all is where this differs from the binary
                // operators, which cannot say the same thing: `is_textual_code`
                // deliberately leaves `VAR_STRING` out because that code also
                // means "not typed at all" here, and calling an *operand* of
                // unknown type approximate would make `unknown + 1` a DOUBLE
                // over a value the evaluator still produces exactly. Under a
                // minus there is nothing to get wrong in that direction:
                // whether the operand is a VARCHAR, a string literal, a hex
                // literal or an expression this cannot type, MySQL's answer is
                // a DOUBLE and never the operand's own code. Measured on 8.3 —
                // `-'abc'`, `-'12'`, `-'12abc'`, `-t` over a VARCHAR, `-vb`
                // over a VARBINARY, `-X'41'`, `-e` over an ENUM, `-CAST('{}' AS
                // JSON)` and `-NULL` are all DOUBLE, as is `-f` over a FLOAT,
                // which widens as `ABS` does. `-d` over a temporal is DOUBLE
                // too, which is worth saying because `d + 0` beside it is a
                // LONGLONG: -20240105 on 8.3.
                U::Minus => match computed_column_type(expr, refs, input, sub) {
                    t if is_integer(t) => ColumnType::LongLong,
                    // `-CAST(1 AS DECIMAL(5,2))` is NEWDECIMAL there: an exact
                    // fraction has a sign already and loses nothing to it.
                    t @ (ColumnType::Decimal | ColumnType::NewDecimal) => t,
                    _ => ColumnType::Double,
                },
                _ => ColumnType::VarString,
            }
        }
        // A cast says what it wants, which is the one place the answer is
        // written down in the statement.
        // `SIGNED` and `UNSIGNED` are spellings a cast accepts and a column
        // declaration does not, so `SqlType::from_ast` has never heard of
        // them and they are answered here.
        Expr::Cast {
            data_type:
                sqlparser::ast::DataType::Signed
                | sqlparser::ast::DataType::SignedInteger
                | sqlparser::ast::DataType::Unsigned
                | sqlparser::ast::DataType::UnsignedInteger,
            ..
        } => ColumnType::LongLong,
        Expr::Cast { data_type, .. } => match crate::types::SqlType::from_ast(data_type) {
            // A cast to CHAR makes a *varying* string: MySQL answers
            // VAR_STRING where a declared `CHAR` column is a `STRING`.
            Ok(t) if t.column_type() == ColumnType::String => ColumnType::VarString,
            Ok(t) => t.column_type(),
            Err(_) => ColumnType::VarString,
        },
        // A CASE is typed by what its branches produce; the first one that is
        // not NULL stands for all of them.
        Expr::Case {
            conditions,
            else_result,
            ..
        } => combined(
            conditions
                .iter()
                .map(|w| &w.result)
                .chain(else_result.as_deref())
                .map(|r| candidate_type(r, refs, input, sub)),
        ),
        // A scalar subquery is the type of the subquery's own projection, not
        // a type of its own: 8.3 answers LONG for `(SELECT sid FROM T)` over
        // an `INT` column, LONGLONG for `(SELECT COUNT(*) FROM T)`, and
        // `binary(0)` for the materialised all-NULL set operation, exactly as
        // it does for the inner query on its own. Which is why this needs the
        // engine — see `Sub` — and why the whole family of walkers here got a
        // parameter for it.
        //
        // `VAR_STRING` where the subquery cannot be described, which is what
        // every scalar subquery was answered before this arm existed.
        Expr::Subquery(query) => subquery_column(sub, query, refs)
            .map(|column| column.ty)
            .unwrap_or(ColumnType::VarString),
        Expr::Nested(inner) => computed_column_type(inner, refs, input, sub),
        _ => ColumnType::VarString,
    }
}

/// Whether a code is one of MySQL's integer types.
fn is_integer(t: ColumnType) -> bool {
    matches!(
        t,
        ColumnType::Tiny
            | ColumnType::Short
            | ColumnType::Int24
            | ColumnType::Long
            // A YEAR is the one temporal type MySQL computes in directly
            // rather than packing first, because it already is a number:
            // `y + 0` is a LONGLONG UNSIGNED there and `y` itself the
            // number the column holds.
            | ColumnType::Year
            | ColumnType::LongLong
    )
}

thread_local! {
    /// Whether the current call is establishing a statement's shape rather
    /// than answering it.
    ///
    /// A thread-local rather than a parameter because it has to reach through
    /// every recursion a FROM clause can take — views, derived tables, CTEs,
    /// scalar subqueries — and threading a flag through all of them would put
    /// the same argument in twenty signatures that do not otherwise care.
    static SHAPING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Restores the shaping flag however the scope is left.
struct Shaping(bool);

impl Drop for Shaping {
    fn drop(&mut self) {
        SHAPING.with(|s| s.set(self.0));
    }
}

/// Runs `f` with stored tables read for their columns and not their rows.
///
/// Restored through a guard rather than a trailing assignment, because the
/// flag left set is the worst possible state: every later query on the thread
/// would read stored tables as empty and answer *successfully* with no rows.
/// A wrong answer, where this engine is supposed to produce a clear error.
fn shaping<T>(f: impl FnOnce() -> T) -> T {
    let _restore = Shaping(SHAPING.with(|s| s.replace(true)));
    f()
}

/// Whether a query's body is a set operation, seen through any brackets the
/// statement was written with.
///
/// `(SELECT NULL UNION SELECT NULL)` parses as a `SetExpr::Query` wrapping the
/// operation, and matching only the bare variant left it typed
/// `MYSQL_TYPE_NULL` where the same statement without the brackets was
/// `binary(0)`. MySQL makes no such distinction: measured on 8.3, brackets at
/// any depth, and an ORDER BY or LIMIT inside or outside them, all give the
/// materialised type. A bracketed *arm* of a larger set operation is a
/// different thing and does not reach here — `set_expr` answers it through
/// `query_with_outer`.
fn is_set_operation(body: &SetExpr) -> bool {
    match body {
        SetExpr::SetOperation { .. } => true,
        SetExpr::Query(inner) => is_set_operation(&inner.body),
        _ => false,
    }
}

/// The column positions a GROUP BY determines, which is more than the columns
/// it names.
///
/// MySQL's ONLY_FULL_GROUP_BY permits a column that is *functionally
/// dependent* on the grouping. `GROUP BY esa.sectionId` with
/// `JOIN es ON es.id = esa.sectionId` fixes `es.id` for the whole group, so
/// selecting it is not ambiguous and MySQL accepts it — the migrations rely on
/// exactly that. Equality conditions in ON and WHERE are followed to build the
/// set; anything they do not reach is still an error.
fn determined_columns(select: &Select, refs: &[ColumnRef], group_exprs: &[Expr]) -> Vec<usize> {
    let mut determined: Vec<usize> = group_exprs
        .iter()
        .filter_map(|e| column_slot(refs, e))
        .collect();

    // Directed: `(from, to)` means a value of `from` fixes `to`.
    let mut equalities: Vec<(usize, usize)> = Vec::new();

    // A WHERE equality holds for every row in the result, so it determines in
    // both directions.
    if let Some(where_clause) = &select.selection {
        for (left, right) in equality_conjuncts(where_clause) {
            if let (Some(l), Some(r)) = (column_slot(refs, &left), column_slot(refs, &right)) {
                equalities.push((l, r));
                equalities.push((r, l));
            }
        }
    }

    for from in &select.from {
        for join in &from.joins {
            let Some(sqlparser::ast::JoinConstraint::On(on)) = join_constraint(&join.join_operator)
            else {
                continue;
            };
            let side = preserved_side(&join.join_operator);
            let joined = factor_name(&join.relation);
            let in_joined = |slot: usize| {
                joined
                    .as_deref()
                    .is_some_and(|name| refs[slot].table.eq_ignore_ascii_case(name))
            };

            for (left, right) in equality_conjuncts(on) {
                let (Some(l), Some(r)) = (column_slot(refs, &left), column_slot(refs, &right))
                else {
                    continue;
                };
                // An outer join's ON does not hold on its NULL-extended rows,
                // so the preserved side is *not* determined by the other one:
                // `a LEFT JOIN b ON a.id = b.id GROUP BY b.id` collects every
                // unmatched `a` into one group. The null-supplying side is
                // determined, because there it is NULL — a single value.
                for (from, to) in [(l, r), (r, l)] {
                    let permitted = match side {
                        PreservedSide::Both => true,
                        PreservedSide::Left => !in_joined(from) && in_joined(to),
                        PreservedSide::Right => in_joined(from) && !in_joined(to),
                        PreservedSide::Neither => false,
                    };
                    if permitted {
                        equalities.push((from, to));
                    }
                }
            }
        }
    }

    // Determination is transitive, so the set is closed rather than walked
    // once. These pairs are directed, so the closure follows them one way.
    let mut grew = true;
    while grew {
        grew = false;
        for (from, to) in &equalities {
            if determined.contains(from) && !determined.contains(to) {
                determined.push(*to);
                grew = true;
            }
        }
    }
    determined
}

/// Which side of a join keeps its rows when the other side has no match.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PreservedSide {
    Both,
    Left,
    Right,
    Neither,
}

fn preserved_side(op: &sqlparser::ast::JoinOperator) -> PreservedSide {
    use sqlparser::ast::JoinOperator as J;
    match op {
        J::Join(_) | J::Inner(_) => PreservedSide::Both,
        J::Left(_) | J::LeftOuter(_) => PreservedSide::Left,
        J::Right(_) | J::RightOuter(_) => PreservedSide::Right,
        // A full outer join preserves both sides, so neither determines the
        // other; a semi or anti join contributes no columns at all.
        _ => PreservedSide::Neither,
    }
}

/// The name a joined table is addressed by, which is its alias where it has one.
fn factor_name(factor: &TableFactor) -> Option<String> {
    match factor {
        TableFactor::Table { name, alias, .. } => Some(
            alias
                .as_ref()
                .map(|a| a.name.value.clone())
                .unwrap_or_else(|| name::dotted(name)),
        ),
        TableFactor::Derived { alias, .. } | TableFactor::JsonTable { alias, .. } => {
            alias.as_ref().map(|a| a.name.value.clone())
        }
        _ => None,
    }
}

/// The `ON` clause of a join, where it has one.
fn join_constraint(op: &sqlparser::ast::JoinOperator) -> Option<&sqlparser::ast::JoinConstraint> {
    use sqlparser::ast::JoinOperator as J;
    match op {
        J::Join(c)
        | J::Inner(c)
        | J::Left(c)
        | J::LeftOuter(c)
        | J::Right(c)
        | J::RightOuter(c)
        | J::FullOuter(c)
        | J::Semi(c)
        | J::LeftSemi(c)
        | J::RightSemi(c)
        | J::Anti(c)
        | J::LeftAnti(c)
        | J::RightAnti(c) => Some(c),
        _ => None,
    }
}

/// Every `a = b` reachable through AND, which is where an equality has to hold
/// for the row to be in the result at all. An OR branch proves nothing.
fn equality_conjuncts(e: &Expr) -> Vec<(Expr, Expr)> {
    use sqlparser::ast::BinaryOperator as Op;
    match e {
        Expr::BinaryOp { left, op, right } => match op {
            Op::Eq => vec![(left.as_ref().clone(), right.as_ref().clone())],
            Op::And => {
                let mut out = equality_conjuncts(left);
                out.extend(equality_conjuncts(right));
                out
            }
            _ => Vec::new(),
        },
        Expr::Nested(inner) => equality_conjuncts(inner),
        _ => Vec::new(),
    }
}

/// Where a column reference lands in the relation, so `sectionId` and
/// `esa.sectionId` are recognised as the same column.
fn column_slot(refs: &[ColumnRef], e: &Expr) -> Option<usize> {
    let written = column_parts(e)?;
    refs.iter().position(|c| c.matches(&written))
}

/// The parts a column reference is written with.
///
/// Split out of `column_slot` because that one stops at the first column a
/// name could mean, and a name that could mean two is a question of its own.
///
/// `name::column_parts` does the splitting, which is where the database part
/// stopped being dropped: this used to read `parts[len - 2]` as the table and
/// nothing before it, so `nodb.J2.b` resolved to `J2`.`b`.
///
/// Parentheses and unary plus are peeled, because neither is written into the
/// reference MySQL resolves: `SELECT b FROM t GROUP BY +b` groups by the column
/// on 8.3, and `SELECT +b FROM t GROUP BY b` answers rather than reporting a
/// nonaggregated column.
fn column_parts(e: &Expr) -> Option<name::ColumnParts<'_>> {
    match peeled(e) {
        Expr::Identifier(i) => Some(name::ColumnParts::bare(i.value.as_str())),
        Expr::CompoundIdentifier(parts) => name::column_parts(parts),
        _ => None,
    }
}

/// Whether a failed probe is the statement's fault rather than the probe's.
///
/// A probe refuses things a real execution would not — a function with
/// consequences, a body that writes — and reporting those at prepare time
/// would fail statements that run perfectly well. It also runs over no rows,
/// so anything a row could have caused is absent rather than proven.
///
/// A name that resolves against nothing is neither. No row and no bound
/// argument can make `a.b` find a table `a` that the FROM clause never named,
/// which is why MySQL raises 1054 and 1051 at prepare time too — and why a
/// client that prepares its statements up front should hear about it there.
fn refused_at_prepare(e: &EngineError) -> bool {
    // ER_BAD_FIELD_ERROR, ER_BAD_TABLE_ERROR and ER_WRONG_ARGUMENTS. Listed
    // rather than matched on a range: every other code this engine raises is
    // either about a value or about the probe, and a new one has to be
    // considered rather than inherited.
    //
    // 1210 is here for the same reason as the other two and not by analogy:
    // `ESCAPE '!!'` names two characters however the statement is run, so no
    // row and no bound argument can make it legal, and 8.3 raises it while
    // resolving the item. Without this line the probe raised it, this said no,
    // and the error was swallowed into `Ok(None)` — so COM_STMT_PREPARE
    // succeeded describing zero columns where MySQL fails the prepare.
    //
    // ER_WRONG_PARAMCOUNT_TO_NATIVE_FCT joins them on the same argument, and
    // more plainly than any of them: how many arguments a call was written
    // with is fixed by the text. Measured, not assumed — `PREPARE p FROM
    // 'SELECT SIGN()'` is 1582 on 8.3.
    matches!(e.code, 1054 | 1051 | 1210 | 1582)
}

/// The first column reference in `e` that nothing in scope answers to.
///
/// Returned as MySQL prints it in 1054 — qualified where it was written
/// qualified, so `SELECT a.b` reports `a.b` and not `b`.
///
/// The evaluator resolves a name only when it evaluates one, and it evaluates
/// one per row. Over no rows nothing is resolved, so the statements this
/// catches are the ones a client prepares, or runs against an empty table, and
/// is told are fine. `refs` is the same scope `expr::eval` would read: this
/// query's relation followed by any enclosing row, so a correlated reference
/// resolves here exactly as it does there.
///
/// Deliberately conservative, and built on `subexpressions` so it stays that
/// way. A variant missing from that list is a place this looks at nothing and
/// answers `None` — the statement is then accepted as it is today. The failure
/// mode is the safe one: refusing a legal query is worse than the gap this
/// closes, because it turns working SQL into an error.
fn unresolved_column(e: &Expr, refs: &[ColumnRef]) -> Option<String> {
    match e {
        // Not columns. `@@GLOBAL.version` parses as a compound identifier, so
        // both spellings have to be let through.
        Expr::Identifier(i) if i.value.starts_with('@') => None,
        Expr::CompoundIdentifier(parts)
            if parts.first().is_some_and(|p| p.value.starts_with('@')) =>
        {
            None
        }
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            column_slot(refs, e).is_none().then(|| written_name(e))
        }
        // A function's arguments are read in this scope, including an
        // aggregate's and a window function's: `SUM(a.b) OVER (ORDER BY b)`
        // is 1054 "in 'field list'" on 8.3.
        //
        // `value_arguments` rather than every argument, because
        // `TIMESTAMPDIFF(DAY, a, b)` writes its unit as a bare keyword that
        // parses as an identifier — refusing it would reject a legal
        // statement for naming a column `DAY`, which the suite caught.
        //
        // An aggregate's own ORDER BY is read in this scope too, and named the
        // same way: `SELECT GROUP_CONCAT(s ORDER BY zz) FROM t` is 1054 "in
        // 'field list'" on 8.3, and it is that over an empty table, under
        // `WHERE 1=0` and with a GROUP BY that makes no groups — so the clause
        // is settled here and not when a row reaches it. The whole clause was
        // being discarded before #0088, so this answered.
        //
        // The OVER clause's own PARTITION BY and ORDER BY are left alone,
        // because this walk carries one clause name for the whole expression
        // and theirs is not the caller's: MySQL names them `window partition
        // by` and `window order by`, and reporting them here would refuse them
        // under the field list's name. `compute_window` names them as it
        // evaluates them, so the message is right for every row that reaches
        // it; resolving them ahead of the first row is #0100's, along with
        // every other clause not yet settled at prepare.
        Expr::Function(f) => value_arguments(f)
            .iter()
            .chain(own_order_by(f).iter().map(|i| &i.expr))
            .find_map(|a| unresolved_column(a, refs)),
        // A subquery resolves its own columns in its own scope, which is why
        // `subexpressions` does not descend into one.
        other => subexpressions(other)
            .into_iter()
            .find_map(|s| unresolved_column(s, refs)),
    }
}

/// A column reference as it was written, for an error message.
///
/// Every part joined, not just the last: MySQL reports `Unknown column
/// 'nodb.J2.b'` and naming the column alone would hide which qualifier was
/// the problem.
fn written_name(e: &Expr) -> String {
    match e {
        Expr::Identifier(i) => i.value.clone(),
        Expr::CompoundIdentifier(parts) => parts
            .iter()
            .map(|p| p.value.as_str())
            .collect::<Vec<_>>()
            .join("."),
        other => other.to_string(),
    }
}

/// The first `LIKE ... ESCAPE` clause in `e` that names anything but one
/// character.
///
/// 8.3 settles the escape while *resolving* the item, not while evaluating it,
/// so a malformed clause is an error even where nothing is ever evaluated:
///
/// ```sql
/// SELECT s LIKE 'a' ESCAPE '!!' FROM L               -- 1210, L empty
/// SELECT CASE WHEN 0 THEN ('a' LIKE 'b' ESCAPE '!!') ELSE 9 END  -- 1210
/// ```
///
/// The evaluator settles it per row, which answers both of those with a
/// result: an empty set for the first and 9 for the second. That is the same
/// shape as the 1054 hole above it — over no rows, or down a branch nothing
/// takes, a check that lives in the row path does not run — and it is fixed
/// the same way, at the same three points, from the same `subexpressions`
/// list.
///
/// Ordered after `unresolved_column` at every one of them, because MySQL
/// resolves names first: `SELECT nope LIKE 'x' ESCAPE '!!' FROM L` is 1054 on
/// 8.3 and not 1210.
///
/// Conservative in the same way, and for the same reason: a variant missing
/// from `subexpressions` is a place this looks at nothing and the statement is
/// accepted as it is today, which is the safe direction.
fn malformed_escape(e: &Expr) -> Option<EngineError> {
    if let Expr::Like { escape_char, .. } | Expr::ILike { escape_char, .. } = e {
        if let Err(bad) = crate::expr::like_escape(escape_char.as_ref()) {
            return Some(bad);
        }
    }
    match e {
        // A call's own expressions are read here too, and `subexpressions`
        // does not carry them.
        Expr::Function(f) => function_carried(f).iter().find_map(malformed_escape),
        other => subexpressions(other).into_iter().find_map(malformed_escape),
    }
}

/// Every expression a function call carries, for the walks that look for
/// something 8.3 settles while resolving the item.
///
/// The arguments, and then the two places a call holds an expression that is
/// not one: an aggregate's own `ORDER BY`, and a window's `PARTITION BY` and
/// `ORDER BY`. 8.3 settles those exactly as it settles an argument — `SELECT
/// GROUP_CONCAT(i ORDER BY SIGN(1, 2)) FROM t` is 1582 there and `SELECT
/// SUM(i) OVER (ORDER BY s LIKE 'a' ESCAPE '!!') FROM t` is 1210 — and both
/// walks answered over neither until this existed.
///
/// `value_arguments` rather than `function_arguments` for the arguments, for
/// the reason that function gives: `TIMESTAMPDIFF(DAY, a, b)` writes a bare
/// unit keyword where an expression would go, and it is not one to descend
/// into.
///
/// A frame's bounds are deliberately absent: `ROWS 2 PRECEDING` takes a
/// literal count and not an expression, so there is nothing there to settle.
fn function_carried(f: &sqlparser::ast::Function) -> Vec<Expr> {
    use sqlparser::ast::WindowType;
    let mut carried = value_arguments(f);
    carried.extend(own_order_by(f).iter().map(|i| i.expr.clone()));
    if let Some(WindowType::WindowSpec(spec)) = &f.over {
        carried.extend(spec.partition_by.iter().cloned());
        carried.extend(spec.order_by.iter().map(|i| i.expr.clone()));
    }
    carried
}

/// The native functions this engine holds a statement to a parameter count
/// for, as `(name, how many)`.
///
/// One entry, because each one is a measurement rather than a reading of the
/// manual, and because the alternative to measuring is worse than the gap:
/// `ABS(1, 2)` answers 1 here today where 8.3 raises 1582, and inventing an
/// arity for every name in `KNOWN` would refuse the optional arguments several
/// of them really have. `SIGN` was measured — 8.3 raises 1582 for `SIGN()`,
/// for `SIGN(1, 2)` and for `SIGN(1, 2, 3)` — and without it the first would
/// answer NULL and the rest would silently ignore the extra arguments.
const FIXED_ARITY: &[(&str, usize)] = &[("sign", 1)];

/// The first call in `e` to a [`FIXED_ARITY`] function with the wrong number
/// of arguments.
///
/// Here rather than in `functions::call` for the reason the escape check above
/// is: `call` runs once per row, so over an empty table, under `WHERE 1=0`, or
/// down a `CASE` branch nothing takes, a refusal placed there does not happen
/// at all and the statement answers where 8.3 refuses.
///
/// The name is echoed as it was written, which is what 8.3 does: `select
/// sign()` says `'sign'` and `select SiGn(1,2)` says `'SiGn'`.
///
/// One ordering gap, measured and left: 8.3 raises 1582 in the *parser*, so it
/// precedes even 1146 — `SELECT SIGN() FROM nosuchtable` is 1582 there, and
/// 1146 here. Beating those would mean checking before the FROM clause is
/// resolved, which is a statement-level pass this engine does not have; this
/// runs where the other resolve-time refusals run.
fn wrong_parameter_count(e: &Expr) -> Option<EngineError> {
    if let Expr::Function(f) = e {
        let written = f.name.to_string();
        let arity = FIXED_ARITY
            .iter()
            .find(|(name, _)| written.eq_ignore_ascii_case(name))
            .map(|(_, arity)| *arity);
        if arity.is_some_and(|arity| function_arguments(f).len() != arity) {
            return Some(EngineError::new(
                1582,
                "42000",
                format!("Incorrect parameter count in the call to native function '{written}'"),
            ));
        }
    }
    match e {
        Expr::Function(f) => function_carried(f).iter().find_map(wrong_parameter_count),
        other => subexpressions(other)
            .into_iter()
            .find_map(wrong_parameter_count),
    }
}

/// The first aggregate in `e` whose own `ORDER BY` names a position no
/// argument of the call answers to.
///
/// The position counts the call's *arguments*, not the query's output columns:
/// `GROUP_CONCAT(s, n ORDER BY 2)` sorts by `n` on 8.3, and `GROUP_CONCAT(s
/// ORDER BY 2)` is `Unknown column '2' in 'order clause'` there. `ORDER BY 0`
/// is the same error, which is why this reads `written_position` rather than
/// `ordinal_of` — and why it quotes the digits back rather than the number.
///
/// Here rather than in the fold for the reason the two refusals above are:
/// with a GROUP BY over an empty table nothing is folded at all, and 8.3 still
/// raises. `AggregateCall::read` raises it too, so the fold does not have to
/// assume this ran.
fn bad_aggregate_order_position(e: &Expr) -> Option<EngineError> {
    if let Expr::Function(f) = e {
        let arguments = value_arguments(f).len();
        for item in own_order_by(f) {
            if let Some((position, written)) = written_position(&item.expr) {
                if position == 0 || position > arguments {
                    return Some(EngineError::unknown_column(written, "order clause"));
                }
            }
        }
    }
    match e {
        Expr::Function(f) => function_carried(f)
            .iter()
            .find_map(bad_aggregate_order_position),
        other => subexpressions(other)
            .into_iter()
            .find_map(bad_aggregate_order_position),
    }
}

/// The first `GROUP_CONCAT` in `e` written as a window function.
///
/// 8.3 has none, and says so with this exact wording whatever the OVER clause
/// holds: a bare `OVER ()`, a `PARTITION BY`, a frame and a named window are
/// all 1235 there. It settles it while resolving the item, not when a row
/// reaches one — `SELECT GROUP_CONCAT('a') OVER ()` with no FROM at all,
/// `... FROM t WHERE 1=0`, an untaken `CASE` branch and an `ORDER BY` are each
/// 1235 — which is why this is here beside the other resolve-time refusals
/// rather than in `compute_window`, where it only covered a projection over a
/// FROM clause and let the other four answer.
///
/// This engine can fold one: every other aggregate here works as a window.
/// Answering where the reference server refuses is still a divergence, so it
/// says what 8.3 says.
fn group_concat_as_window(e: &Expr) -> Option<EngineError> {
    if let Expr::Function(f) = e {
        if f.over.is_some() && f.name.to_string().eq_ignore_ascii_case("group_concat") {
            return Some(EngineError::new(
                1235,
                "42000",
                "This version of MySQL doesn't yet support 'group_concat as window function'",
            ));
        }
    }
    match e {
        Expr::Function(f) => function_carried(f).iter().find_map(group_concat_as_window),
        other => subexpressions(other)
            .into_iter()
            .find_map(group_concat_as_window),
    }
}

/// The collation name in `SET collation_connection = utf8mb4_general_ci`,
/// where it is written without quotes.
///
/// MySQL takes a bare identifier on the right of a `SET` as the *name* it is
/// written as; there is no table in scope, so it can never be a column
/// reference. This engine evaluates it as an expression and answers 1054,
/// `Unknown column 'utf8mb4_general_ci' in 'field list'` — measured against
/// 8.3, which sets the variable.
///
/// Narrowed to this one variable on purpose, rather than fixed for `SET` in
/// general. The general form is #0095's, and doing it here would turn a loud
/// refusal into a silent wrong answer for the variables this engine reports
/// and does not honour: `SET foreign_key_checks = OFF` would start succeeding
/// while foreign keys went on being checked. The connection's collation is
/// different because the engine now *reads* it, so accepting the spelling and
/// acting on it are the same thing.
///
/// A *user* variable of the same name is not this: `SET @collation_connection
/// = utf8mb4_bin` is 1054 on 8.3, because the right-hand side of a
/// user-variable assignment really is an expression. Only a bare name or an
/// `@@`-prefixed one is a system variable.
fn unquoted_collation(variable: &str, value: &Expr) -> Option<Value> {
    // Only `@@` is stripped, which is what keeps a *user* variable out: `SET
    // @collation_connection = utf8mb4_bin` keeps its `@` here, so the name
    // below cannot match and the assignment goes on being an expression — 1054
    // on 8.3, measured beside the system-variable form.
    let system = variable.strip_prefix("@@").unwrap_or(variable);
    let name = system.to_ascii_lowercase();
    // A `GLOBAL.` / `SESSION.` / `LOCAL.` prefix names the same variable here,
    // as `Session::set_system_variable` also resolves it.
    let name = name.rsplit('.').next().unwrap_or(&name);
    if name != "collation_connection" {
        return None;
    }
    match value {
        // `DEFAULT` is the one bare identifier that is not a name: 8.3 resets
        // the variable to the session default, measured — `SET
        // collation_connection = DEFAULT` puts `utf8mb4_0900_ai_ci` back.
        Expr::Identifier(ident) if ident.value.eq_ignore_ascii_case("default") => {
            Some(Value::text(crate::collation::Collation::default().name()))
        }
        Expr::Identifier(ident) if !ident.value.starts_with('@') => {
            Some(Value::text(ident.value.clone()))
        }
        _ => None,
    }
}

/// Refuses a clause 8.3 settles before it evaluates a row: an ESCAPE that
/// cannot be one, a native function called with the wrong parameter count, an
/// aggregate's own `ORDER BY` naming a position it has no argument for, and
/// `GROUP_CONCAT` written as a window function.
///
/// One function rather than the `if let Some(bad)` written out at each clause,
/// because the number of clauses is the whole problem this has: #0093 wired
/// the check into five of them and shipped with HAVING, GROUP BY, ON and
/// every DML predicate still settling it per row, which over no rows is not
/// settling it at all. A clause that needs it says so in one line here, and
/// this stays the single point a mutation can neuter to prove every one of
/// them at once.
///
/// `pub(crate)` for `ddl.rs`, which needs it for the same reason and one step
/// earlier: a generated column, a default and a CHECK are expressions stored
/// as text and evaluated per row, so a declaration carrying one of these is
/// settled by the CREATE or never.
///
/// Within one expression the parameter count is asked first, because 8.3 asks
/// it first: `SELECT ('a' LIKE 'b' ESCAPE '!!') AND SIGN() > 0` is 1582 there
/// and not 1210. Across a *list* of them the first item's problem still wins,
/// so `SELECT s LIKE 'a' ESCAPE '!!', SIGN() FROM t` is 1210 here and 1582 on
/// 8.3 — the same ordering gap `wrong_parameter_count` records against 1146,
/// and for the same reason. Both answers are refusals, which is the half that
/// matters.
///
/// The ORDER BY position is asked last, which is also 8.3's order within one
/// expression: `SELECT CONCAT(GROUP_CONCAT(s ORDER BY 2), SIGN()) FROM t` is
/// 1582 there and `SELECT GROUP_CONCAT(s ORDER BY 2, s LIKE 'a' ESCAPE '!!')
/// FROM t` is 1210, and both are those here. An unresolvable *name* beats it
/// as well — `ORDER BY 2, zz` is 1054 for `zz` — which falls out of
/// `resolve_projected_expr` asking `unresolved_column` before it calls this.
/// Across a list it is the same gap as above: `SELECT GROUP_CONCAT(s ORDER BY
/// 2), SIGN() FROM t` is 1582 on 8.3 and 1054 here, because the first item's
/// problem wins.
///
/// `GROUP_CONCAT` as a window function is asked last of all, which is 8.3's
/// order again: measured against each of the three above, 1582, 1210 and the
/// order-clause 1054 all beat the 1235 — `SELECT GROUP_CONCAT('a' ORDER BY 2)
/// OVER ()` is `Unknown column '2' in 'order clause'` there and not 1235 —
/// and so does a name, which `resolve_projected_expr` settles first.
pub(crate) fn refuse_at_resolve<'a>(
    clauses: impl IntoIterator<Item = &'a Expr>,
) -> EngineResult<()> {
    let refusal = |e: &Expr| -> Option<EngineError> {
        wrong_parameter_count(e)
            .or_else(|| malformed_escape(e))
            .or_else(|| bad_aggregate_order_position(e))
            .or_else(|| group_concat_as_window(e))
    };
    match clauses.into_iter().find_map(refusal) {
        Some(bad) => Err(bad),
        None => Ok(()),
    }
}

/// The database and table a `db.tbl.*` names, unquoted.
///
/// Read off the identifiers rather than the rendered text. Display keeps an
/// identifier's quotes, so a back-quoted qualifier came back with the quotes
/// still on it — which matches no table, and once the check in
/// `resolve_field_list` existed refused a statement MySQL answers. Split by
/// `name::qualifier` rather than on a dot in the joined text, which discarded
/// the database and could not tell it from a dot inside a name.
///
/// `None` where there is no database and table to be had: more parts than the
/// grammar allows, or the `expr.*` form.
fn wildcard_qualifier(
    kind: &sqlparser::ast::SelectItemQualifiedWildcardKind,
) -> Option<(Option<String>, String)> {
    match kind {
        sqlparser::ast::SelectItemQualifiedWildcardKind::ObjectName(name) => name::qualifier(name),
        // `expr.*`, which only BigQuery and Snowflake parse —
        // `supports_select_expr_star` is false for MySQL, so this arm is
        // unreachable here and names nothing rather than inventing a table for
        // something that is not a qualifier at all.
        _ => None,
    }
}

/// The `db.tbl` a qualified wildcard was written with, for its 1051 message.
///
/// Joined from the parts rather than rendered, so a back-quoted qualifier is
/// reported the way 8.3 reports it — as a name, without the quotes.
fn wildcard_written(kind: &sqlparser::ast::SelectItemQualifiedWildcardKind) -> String {
    match kind {
        sqlparser::ast::SelectItemQualifiedWildcardKind::ObjectName(name) => name::dotted(name),
        other => other.to_string().trim_end_matches(".*").to_owned(),
    }
}

/// Resolves one select item's expression: its names, and then the arguments
/// 8.3 settles while resolving rather than while evaluating.
///
/// Ahead of every other reading of the item, because the expression path only
/// resolves a name when it evaluates one, and it evaluates one per row. With
/// no rows — an empty table, a filter nothing survived, or a statement being
/// described — `SELECT a.b FROM J2` declared a column called `b` and returned
/// successfully.
///
/// One definition for the two paths that need it: `resolve_field_list` below
/// for a query with a FROM, and `select_without_table` for one without. A
/// check added to either has to reach both, and a mutation to this neuters
/// them together.
fn resolve_projected_expr(e: &Expr, refs: &[ColumnRef]) -> EngineResult<()> {
    if let Some(name) = unresolved_column(e, refs) {
        return Err(EngineError::unknown_column(&name, "field list"));
    }
    refuse_at_resolve([e])
}

/// Resolves the WHERE of an UPDATE or a DELETE, before any row is considered.
///
/// The DML half of what `resolve_projected_expr` does for a select list, and
/// the same hole: a predicate is read once per row, so over an empty table
/// nothing in it is resolved at all and `UPDATE Bare SET k = 1 WHERE a.b = 1`
/// reported nought rows affected where 8.3 raises 1054. On a write that
/// silence is the worst spelling of it — the caller cannot tell "nothing
/// matched" from "the statement was illegal".
///
/// `'where clause'`, which is where 8.3 puts it and *not* where this engine
/// used to: the per-row path raised the name through the evaluator, which
/// reports every unknown name as being in the field list, so `DELETE FROM S
/// WHERE nope = 1` over a table with rows in it said `in 'field list'` where
/// 8.3 says `in 'where clause'`.
///
/// One definition for the four entry points that have a WHERE — `update`,
/// `update_joined`, `delete` and `delete_joined` — so a mutation to it neuters
/// every one of them at once, and a clause the next statement form needs is a
/// single line.
///
/// `INSERT ... ON DUPLICATE KEY UPDATE` is the write this does *not* reach,
/// and it has no WHERE to reach: #0165 closed the same hole there with
/// `resolve_assigned_values` alone, at the call site in `insert`, where the
/// scope is written out. It stayed separate rather than being folded in
/// because the scope is not this one — the clause resolves `VALUES(x)` too,
/// and for an `INSERT ... SELECT` it sees the SELECT's FROM tables as well, so
/// resolving it against the target table alone would refuse legal SQL.
fn resolve_dml_where(selection: Option<&Expr>, refs: &[ColumnRef]) -> EngineResult<()> {
    let Some(predicate) = selection else {
        return Ok(());
    };
    if let Some(name) = unresolved_column(predicate, refs) {
        return Err(EngineError::unknown_column(&name, "where clause"));
    }
    // After the name, because 8.3 resolves names first: `UPDATE LE SET s = 'x'
    // WHERE nope = 1 AND s LIKE 'a' ESCAPE '!!'` is 1054 there and was 1210
    // here — the ordering #0087 recorded as this issue's to correct.
    refuse_at_resolve([predicate])
}

/// Resolves the names a GROUP BY writes, before any row is grouped.
///
/// The clause is otherwise only read while a key is being built, once per row,
/// so a query producing no groups never read it at all: over an empty table
/// `SELECT k FROM Cl GROUP BY zz.q` came back as an empty set where 8.3 raises
/// 1054, and a `COM_STMT_PREPARE` of it succeeded where 8.3 refuses. #0100.
///
/// `'group statement'` — MySQL's name for this clause in this message, and not
/// the `'group clause'` the pattern of the others would suggest.
///
/// What comes back is the clause with its positions and its select aliases
/// resolved, because admitting a name is only half of reading it: #0100 put
/// the select list's output names in scope here so the clause could not refuse
/// ahead of the first row what a row would have allowed, but the row that then
/// arrived still looked for a column of that name and raised the 1054 this had
/// just declined to raise. #0229. Handing the resolved list back is what makes
/// the two halves one decision — the caller groups on what this resolved, so a
/// name cannot be admitted here and lost there.
///
/// Item by item, because 8.3 reads the list left to right and answers whatever
/// is wrong with the first item that is — a position, a name, an ambiguity or
/// an argument, whichever the item carries. Measured both ways, over a table
/// with rows and over an empty one, with the same answer each time:
///
/// ```sql
/// GROUP BY nope, s LIKE 'a' ESCAPE '!!'   -- 1054
/// GROUP BY s LIKE 'a' ESCAPE '!!', nope   -- 1210
/// GROUP BY a, 9        -- 1052 for the ambiguous alias, over two select items
/// GROUP BY 9, a        -- 1054 for the position
/// GROUP BY n, s LIKE 'a' ESCAPE '!!'      -- 1056, for `n` an aggregate alias
/// ```
///
/// Checking every name and then every argument gets the second of those
/// wrong, and it is the shape the per-row path already answered correctly —
/// so a whole-clause pass would have made a settled statement worse to settle
/// an unsettled one. Within *one* item the two are still separate walks, and
/// merging them is #0087's.
///
/// One definition for the two paths that group — `grouped` for a query with a
/// FROM and `select_without_table` for one without — so a mutation to it
/// neuters both at once.
fn resolve_group_by<E: std::borrow::Borrow<Expr>>(
    written_groups: &[Expr],
    projection: &[SelectItem],
    items: &[(String, E)],
    refs: &[ColumnRef],
) -> EngineResult<Vec<Expr>> {
    // The select list's output names beside the relation's columns, for the
    // resolution below. Nearly every alias is substituted before it gets
    // there — but `substitute_names` reaches an identifier through an
    // operator and not through a call, so `GROUP BY ABS(a)` arrives with its
    // `a` intact. 8.3 resolves an alias at any depth, so refusing that name
    // would answer 1054 where 8.3 answers 1055 for the ungrouped column
    // behind it: measured, `SELECT k AS a, COUNT(*) FROM Cl GROUP BY ABS(a)`
    // is 1055 over a full table and over an empty one alike, and so is the
    // same query over `CONCAT(a, '')`.
    let mut named = refs.to_vec();
    named.extend(items.iter().map(|(alias, _)| ColumnRef::new("", alias)));

    let mut resolved = Vec::with_capacity(written_groups.len());
    for written in written_groups {
        // The position first, because a position out of range beats an alias
        // written after it and loses to one written before: `GROUP BY 9, a` is
        // 1054 for `9` on 8.3 and `GROUP BY a, 9` is 1052 for `a`.
        //
        // Zero is a position too, and out of range: `resolve_ordinal` leaves
        // an unresolvable one as the literal it was written as, which groups
        // every row under one constant key and answers a different query than
        // the one asked. Both ends are 1054 under this clause's own name on
        // 8.3 — `GROUP BY 5` over one select item, and `GROUP BY 0`.
        let at = written_position(written);
        if let Some((position, digits)) = at {
            if position == 0 || position > items.len() {
                return Err(EngineError::unknown_column(digits, "group statement"));
            }
        }
        let e = match resolve_ordinal(written, projection) {
            Some(item) => {
                // A position reaches the same select item an alias does, so it
                // meets the same rule about what can be grouped on — and it is
                // reported by the item's *output name* rather than by the
                // digits. Measured: `SELECT COUNT(*) AS n FROM Cl GROUP BY 1`
                // is `Can't group on 'n'` on 8.3, and the same query with the
                // alias left off is `Can't group on 'COUNT(*)'`.
                if let Some((name, _)) = at.and_then(|(position, _)| items.get(position - 1)) {
                    refuse_ungroupable(&item, name)?;
                }
                item
            }
            None => substitute_group_aliases(written, items, refs)?,
        };
        if let Some(name) = unresolved_column(&e, &named) {
            return Err(EngineError::unknown_column(&name, "group statement"));
        }
        refuse_at_resolve([&e])?;
        resolved.push(e);
    }
    Ok(resolved)
}

/// Refuses a select item a GROUP BY cannot group on.
///
/// An aggregate is folded over the group and a window is computed across the
/// whole partition once the groups exist, so neither can be a key: 8.3 answers
/// `Can't group on '<name>'` — 1056 — rather than the 1054 an unknown name
/// gets or the 1055 an ungrouped column gets.
///
/// One definition for the two spellings that reach it, an alias and a
/// position, so a mutation to it neuters both at once. `<name>` is the item's
/// output name in each — the alias where it has one, the text it was written
/// as where it does not.
fn refuse_ungroupable(e: &Expr, name: &str) -> EngineResult<()> {
    if contains_aggregate(e) || contains_window(e) {
        return Err(EngineError::new(
            1056,
            "42000",
            format!("Can't group on '{name}'"),
        ));
    }
    Ok(())
}

/// Rewrites a GROUP BY's references to a select-list item into the expression
/// written for it.
///
/// The relation's columns win, which is the preference 8.3 documents for this
/// clause and the opposite of the one ORDER BY has. Measured: `SELECT k AS b
/// FROM Cl GROUP BY b`, over a table that has a column `b` of its own, groups
/// on the *column* and is 1055 for the ungrouped `k` — and `SELECT Cl.k AS s
/// ... GROUP BY s` is the same. Only a name the relation cannot answer to
/// reaches the select list at all.
fn substitute_group_aliases<E: std::borrow::Borrow<Expr>>(
    e: &Expr,
    items: &[(String, E)],
    refs: &[ColumnRef],
) -> EngineResult<Expr> {
    // The walk cannot fail, so the ambiguity and the aggregate are carried out
    // of it rather than returned from it. First one wins, which is the same
    // left-to-right reading the loop above does.
    let failure: std::cell::RefCell<Option<EngineError>> = std::cell::RefCell::new(None);
    let record = |error| {
        let mut slot = failure.borrow_mut();
        if slot.is_none() {
            *slot = Some(error);
        }
    };
    let substituted = substitute_names(e, &|written| {
        // The select list is searched *before* the relation answers, even
        // though the relation wins: an alias written twice is ambiguous
        // whether the table has a column of that name or not. Measured, over
        // a `Cl` that has a column `b`: `SELECT k AS b, s AS b FROM Cl GROUP
        // BY b` is 1052 on 8.3, where the single-alias `SELECT k AS b FROM Cl
        // GROUP BY b` groups on the column and is 1055 for the ungrouped `k`.
        let found = match group_alias(written, items, refs) {
            Ok(found) => found,
            Err(error) => {
                record(error);
                return None;
            }
        };
        let taken_by_the_relation =
            column_slot(refs, &Expr::Identifier(sqlparser::ast::Ident::new(written))).is_some();
        if taken_by_the_relation {
            return None;
        }
        let found = found?;
        // Asked of whichever expression won, and so not asked at all when the
        // relation took the name: `GROUP BY b` beside `SELECT COUNT(*) AS b`
        // is 1056 only where nothing is called `b` in the FROM.
        if let Err(error) = refuse_ungroupable(&found, written) {
            record(error);
            return None;
        }
        Some(found)
    });
    match failure.into_inner() {
        Some(error) => Err(error),
        None => Ok(substituted),
    }
}

/// The select item a GROUP BY name resolves to, once the relation has declined
/// it.
///
/// 8.3 walks the select list and treats a plain column reference differently
/// from everything else, which is visible from outside and had to be measured
/// rather than guessed. A second *column* under the same alias is ambiguous;
/// anything else settles the name where it stands and stops the walk. All four
/// orders, over `Cl(id, k, b, s)`:
///
/// ```sql
/// SELECT k AS a, b AS a, COUNT(*) FROM Cl GROUP BY a  -- 1052, two columns
/// SELECT k AS a, k AS a, COUNT(*) FROM Cl GROUP BY a  -- answers; the same one twice
/// SELECT k AS a, 2 AS a, COUNT(*) FROM Cl GROUP BY a  -- groups on 2
/// SELECT 2 AS a, k AS a, COUNT(*) FROM Cl GROUP BY a  -- groups on 2
/// ```
///
/// The last two are 1055 there for the ungrouped `k`, which is how the grouped
/// expression is visible at all — the constant won in both, so the walk does
/// not stop at the first match and does not prefer a column either.
///
/// Two items that are the same column are not ambiguous however they are
/// spelled, which is the rule `OutItem` states for ORDER BY and which this
/// clause shares: `SELECT Cl.k AS a, k AS a, COUNT(*) FROM Cl GROUP BY a`
/// answers on 8.3, where the two *different* columns above are 1052. So the
/// comparison is which column each item resolves to and not how it was
/// written; where neither resolves to one — a correlated reference, or a
/// query with no FROM — the spelling is all there is to compare.
///
/// The ambiguity is the only thing this raises. Whether the item can be
/// grouped on at all is the caller's, because it is asked of whatever finally
/// won the name and the relation may still take it.
fn group_alias<E: std::borrow::Borrow<Expr>>(
    written: &str,
    items: &[(String, E)],
    refs: &[ColumnRef],
) -> EngineResult<Option<Expr>> {
    let mut found: Option<&Expr> = None;
    for (name, item) in items {
        let e = item.borrow();
        if !name.eq_ignore_ascii_case(written) {
            continue;
        }
        if matches!(e, Expr::Identifier(_) | Expr::CompoundIdentifier(_)) {
            match found {
                Some(previous) if !same_column(previous, e, refs) => {
                    return Err(EngineError::ambiguous(written, "group statement"))
                }
                Some(_) => {}
                None => found = Some(e),
            }
        } else {
            found = Some(e);
            break;
        }
    }
    Ok(found.cloned())
}

/// Whether two select items name the same column of the same relation.
fn same_column(left: &Expr, right: &Expr, refs: &[ColumnRef]) -> bool {
    match (column_slot(refs, left), column_slot(refs, right)) {
        (Some(a), Some(b)) => a == b,
        _ => left == right,
    }
}

/// Resolves the ORDER BY of an UPDATE or a DELETE, before any row is chosen.
///
/// The last clause of a DML statement to be settled per row rather than ahead
/// of them, and the worst place for it: the clause decides *which* rows are
/// written, so a statement whose WHERE matched nothing never read it at all.
/// Measured on 8.3 through `COM_STMT_PREPARE`, which refuses all three before
/// the statement is ever executed:
///
/// ```sql
/// UPDATE Bare SET k = 1 WHERE id = 4242 ORDER BY zz.q LIMIT 1  -- 1054, was 0 rows
/// UPDATE LE SET s = 'x' ORDER BY s LIKE 'a' ESCAPE '!!'        -- 1210, was applied
/// UPDATE LE SET s = 'x' ORDER BY SIGN()                        -- 1582, was applied
/// ```
///
/// `'order clause'`, measured, and not the `'field list'` the evaluator
/// reported when a row did happen to reach it.
///
/// Last of the DML resolvers, which is 8.3's order: with a bad ORDER BY beside
/// a bad anything else, the other clause is named. Placed at the top of
/// `dml_row_order` so the two statements that have the clause share one
/// definition and a mutation to it neuters both.
///
/// No select list means no output alias for the clause to admit, which is the
/// trap a SELECT's ORDER BY has and this one does not: `UPDATE t SET k = 1
/// ORDER BY k` names a column of the table and nothing else can be named.
///
/// Item by item, as `resolve_group_by` above is and for the same measurement:
/// 8.3 reads the list left to right, so `ORDER BY nope, s LIKE 'a' ESCAPE
/// '!!'` is 1054 and `ORDER BY s LIKE 'a' ESCAPE '!!', nope` is 1210. Every
/// name and then every argument gets the second wrong — and gets it wrong
/// where the per-row path had it right, which would have been a regression
/// rather than a gap left open.
fn resolve_dml_order(order_by: &[OrderByExpr], refs: &[ColumnRef]) -> EngineResult<()> {
    for item in order_by {
        if let Some(name) = unresolved_column(&item.expr, refs) {
            return Err(EngineError::unknown_column(&name, "order clause"));
        }
        refuse_at_resolve([&item.expr])?;
    }
    Ok(())
}

/// Resolves the values an assignment list writes, before any row is
/// considered.
///
/// The other half of the same hole, and the one #0102 leads with: `UPDATE Bare
/// SET k = a.b` over an empty table affected nought rows where 8.3 raises
/// 1054. `'field list'`, measured — 8.3 names an assigned value's clause the
/// same way it names a selected one's, and it names an `ON DUPLICATE KEY
/// UPDATE` value's the same way again.
///
/// Two call sites, and they place it on *opposite* sides of the assignment
/// targets, because that is where 8.3 puts it in each:
///
/// - From `update`, above the targets, where #0087 put the ESCAPE guard. 8.3
///   settles the targets first, so a statement whose target and whose value
///   are both unknown is reported here by its value where 8.3 reports it by
///   its target: `UPDATE Bare SET k = nope1, nope2 = 1` names `nope1` and 8.3
///   names `nope2`. Both are 1054 in the field list and the difference is
///   which name the message carries, where moving it below the targets would
///   trade that for a difference in *code* — the generated-column refusal
///   lives in that loop, and `UPDATE Gn SET g = 1, k = nope` is 1054 on 8.3
///   and not 3105.
/// - From `insert`, below them, which is 8.3's order for that clause and not
///   the same trade: `INSERT INTO Bare (id, k) VALUES (1, 1) ON DUPLICATE KEY
///   UPDATE k = zz2, zz1 = 1` names `zz1` there, and `... UPDATE g = 1, k =
///   nope` names `nope` rather than answering 3105 — so nothing is given up by
///   letting every target resolve first.
///
/// Per assignment rather than every name and then every ESCAPE, because that
/// is how the select list settles the same pair: `SELECT ('a' LIKE 'b' ESCAPE
/// '!!'), nope FROM t` is 1210 on 8.3 and the first item's problem wins. Both
/// clauses were measured for it — `SET label = ('a' LIKE 'b' ESCAPE '!!'), sid
/// = nope` is 1210 and the same two assignments the other way round are 1054,
/// and so are the two spellings of the `ON DUPLICATE KEY UPDATE` pair.
fn resolve_assigned_values(assignments: &[Assignment], refs: &[ColumnRef]) -> EngineResult<()> {
    for assignment in assignments {
        // `SET k = DEFAULT` writes the column's declared default, and the
        // parser hands it over as a bare identifier indistinguishable from a
        // column called `DEFAULT` — the same shape `writes_nothing` has to
        // recognise on the INSERT path. Resolving it as a name refused a
        // statement 8.3 accepts, which is the direction that matters: over an
        // empty table `UPDATE Bare SET k = DEFAULT` answered nothing affected
        // before this guard existed and 1054 with it.
        //
        // Exempted rather than implemented. The evaluator still has no
        // meaning for it, so the same statement over a table *with* rows in it
        // is 1054 here as it was before — a gap of its own, and one this
        // change must not widen rather than one it can close in passing.
        if is_default_keyword(&assignment.value) {
            continue;
        }
        if let Some(name) = unresolved_column(&assignment.value, refs) {
            return Err(EngineError::unknown_column(&name, "field list"));
        }
        refuse_at_resolve([&assignment.value])?;
    }
    Ok(())
}

/// Whether an expression is the bare `DEFAULT` keyword.
///
/// It parses as an identifier, so nothing about the AST distinguishes it from
/// a column of that name; every walk that reads a written value has to know.
fn is_default_keyword(e: &Expr) -> bool {
    matches!(e, Expr::Identifier(i) if i.value.eq_ignore_ascii_case("default"))
}

/// Resolves every name and every ESCAPE in a select list.
///
/// Its own pass rather than a line inside the projection loop, because the
/// order matters and something now runs between the two: 8.3 settles the field
/// list, then the HAVING, then computes the windows, and the HAVING decides
/// which rows a window even sees. `SELECT nope FROM t HAVING zz > 1` is 1054
/// for `nope` there, so this has to happen before `filter_having` and the
/// projection has to happen after it.
///
/// Called from `project` too, where these checks used to live, so a statement
/// that never reaches the HAVING is resolved exactly as it was.
fn resolve_field_list(
    projection: &[SelectItem],
    refs: &[ColumnRef],
    relation: &Relation,
) -> EngineResult<()> {
    for item in projection {
        match item {
            SelectItem::Wildcard(_) => {}
            SelectItem::QualifiedWildcard(kind, _) => {
                // A qualifier this query's own FROM clause does not provide is
                // 1051, not an empty column list: `SELECT a.* FROM J2` and
                // `SELECT J2.* FROM J2 x` both raise it on 8.3, and this
                // engine used to return a result set with no columns in it at
                // all. The database beside the table is part of what the FROM
                // clause has to provide — `SELECT nodb.J2.* FROM J2` is 1051
                // there and was every column of `J2` here.
                //
                // Asked of the relation and not of `refs`, because a `*` does
                // not see the enclosing query the way a named column does:
                // `SELECT id FROM A WHERE 1 IN (SELECT A.* FROM B)` is
                // `Unknown table 'A'` on 8.3 even though `A` is right there in
                // the outer FROM. The one place an out-of-scope qualifier is
                // legal is inside EXISTS, where the select list is discarded
                // before any `*` in it is expanded — handled where that
                // happens, in `expr::eval`.
                let named = wildcard_qualifier(kind).is_some_and(|(database, table)| {
                    relation.has_source(database.as_deref(), &table)
                });
                if !named {
                    return Err(EngineError::bad_table(&wildcard_written(kind)));
                }
            }
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                resolve_projected_expr(e, refs)?;
            }
            // Refused where the projection is built, because that is a gap in
            // this engine rather than a question about the statement.
            _ => {}
        }
    }
    Ok(())
}

/// Rejects a HAVING that names something the clause cannot see.
///
/// HAVING does not read the FROM clause. Measured on 8.3: a name in it
/// resolves against the select list, the GROUP BY columns and any enclosing
/// query — and nothing else, whether or not ONLY_FULL_GROUP_BY is on. `SELECT
/// b FROM t HAVING id > 1` is 1054 "in 'having clause'" there with `id` in
/// plain sight in `t`, and so is `SELECT COUNT(*) FROM t HAVING b > 1`, which
/// this engine answered as an empty set — the group's first row happened not
/// to pass. Only an aggregate's argument, and a subquery, read the relation.
///
/// It is what keeps the routing change honest. Sending a HAVING with no
/// aggregate down the per-row path makes every column of the relation reachable
/// from the clause, so without this the two statements above would have become
/// answers rather than better errors.
fn check_having_scope(
    having: &Expr,
    projection: &[SelectItem],
    group_exprs: &[Expr],
    refs: &[ColumnRef],
    own: usize,
) -> EngineResult<()> {
    let recur = |e: &Expr| check_having_scope(e, projection, group_exprs, refs, own);
    match having {
        // Ahead of everything else in the walk, and the walk is left to right,
        // because that is the order 8.3 answers in: `HAVING ROW_NUMBER() OVER
        // () > 1 AND zz > 1` is 3593 and `HAVING zz > 1 AND ROW_NUMBER() OVER
        // () > 1` is 1054.
        Expr::Function(f) if f.over.is_some() => {
            Err(EngineError::window_function_misplaced(&f.name.to_string()))
        }
        // An aggregate's argument *is* read against the relation: `SELECT b
        // FROM t HAVING SUM(id) > 1` resolves `id` on 8.3 and refuses on the
        // select list instead. One that resolves against nothing at all is
        // still 1054, under this clause's name.
        //
        // The call's own ORDER BY is read the same way and named the same way:
        // `HAVING GROUP_CONCAT(s ORDER BY zz) = 'x'` is 1054 "in 'having
        // clause'" on 8.3. Without it here the name reached the fold and came
        // back under `field list`, which is the clause it is not in.
        Expr::Function(f) if is_aggregate(&f.name.to_string()) => value_arguments(f)
            .iter()
            .chain(own_order_by(f).iter().map(|i| &i.expr))
            .find_map(|a| unresolved_column(a, refs))
            .map_or(Ok(()), |name| {
                Err(EngineError::unknown_column(&name, "having clause"))
            }),
        // Variables, not columns — the same two spellings `unresolved_column`
        // has to let through.
        Expr::Identifier(i) if i.value.starts_with('@') => Ok(()),
        Expr::CompoundIdentifier(parts)
            if parts.first().is_some_and(|p| p.value.starts_with('@')) =>
        {
            Ok(())
        }
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => {
            resolve_having_name(having, projection, group_exprs, refs, own)
        }
        Expr::Function(f) => value_arguments(f).iter().try_for_each(recur),
        // `subexpressions` does not descend into a subquery, which is what
        // makes the correlated case right: a subquery resolves its own names
        // in its own scope, and the outer columns it reaches are checked when
        // that query is run.
        other => subexpressions(other).into_iter().try_for_each(recur),
    }
}

/// Where one name written in a HAVING resolves, or why it does not.
///
/// Resolution is by identity rather than by spelling, which is what 8.3 does:
/// `SELECT b AS zzz FROM t HAVING b > 1` answers there — the clause found the
/// column the select list projects, under neither of the names on it — and so
/// does `SELECT b FROM t HAVING t.b > 1`. A select-list alias wins over a
/// column, so `SELECT id AS b FROM t HAVING b > 1` filters on `id`.
fn resolve_having_name(
    e: &Expr,
    projection: &[SelectItem],
    group_exprs: &[Expr],
    refs: &[ColumnRef],
    own: usize,
) -> EngineResult<()> {
    let slot = column_slot(refs, e);
    // Only an unqualified name can name an alias: `SELECT id AS b FROM t
    // HAVING t.b > 1` is 1054 on 8.3, because `t.b` asks for a column of `t`
    // and the alias is not one.
    let bare = match e {
        Expr::Identifier(i) => Some(i.value.as_str()),
        _ => None,
    };

    // Every own column a `*` could have projected under this name, not just
    // the first one `column_slot` finds: `SELECT * FROM t a JOIN t c ON a.id =
    // c.id HAVING id > 1` puts two columns called `id` in the select list and
    // is 1052 on 8.3, and resolving to either of them would have answered it.
    let projected_slots = |from: Option<(Option<&str>, &str)>| -> Vec<usize> {
        let Some(written) = column_parts(e) else {
            return Vec::new();
        };
        refs.iter()
            .take(own)
            .enumerate()
            .filter(|(_, c)| {
                c.matches(&written)
                    && from.is_none_or(|(database, table)| c.in_source(database, table))
            })
            .map(|(i, _)| i)
            .collect()
    };

    // What the name could resolve to, one entry per candidate, each carrying
    // whether the expression behind it is a window.
    let mut found: Vec<(String, bool)> = Vec::new();
    for item in projection {
        match item {
            // A `*` projects every column of this query's own relation, so a
            // name that lands in one of them is in the select list without any
            // item having been written for it.
            SelectItem::Wildcard(_) => {
                for slot in projected_slots(None) {
                    found.push((format!("#{slot}"), false));
                }
            }
            SelectItem::QualifiedWildcard(kind, _) => {
                // A qualifier this cannot split names no FROM item, so it
                // projects nothing into the select list for a HAVING to find.
                if let Some((database, table)) = wildcard_qualifier(kind) {
                    for slot in projected_slots(Some((database.as_deref(), &table))) {
                        found.push((format!("#{slot}"), false));
                    }
                }
            }
            SelectItem::UnnamedExpr(inner) | SelectItem::ExprWithAlias { expr: inner, .. } => {
                let name = match item {
                    SelectItem::ExprWithAlias { alias, .. } => name::alias(alias),
                    _ => render_expr(inner),
                };
                let inner_slot = column_slot(refs, inner);
                let by_name = bare.is_some_and(|n| name.eq_ignore_ascii_case(n));
                let by_column = slot.is_some() && inner_slot == slot;
                if by_name || by_column {
                    let identity = inner_slot
                        .map(|s| format!("#{s}"))
                        .unwrap_or_else(|| render_expr(inner));
                    found.push((identity, contains_window(inner)));
                }
            }
            _ => {}
        }
    }

    // Two candidates that are the same thing are not ambiguous, which is why
    // this compares identities rather than counting matches: `SELECT t.b, b
    // FROM t HAVING b > 1` answers on 8.3 and `SELECT id AS b, t.b FROM t
    // HAVING b > 1` is 1052.
    if found.len() > 1 && found.iter().any(|(id, _)| *id != found[0].0) {
        return Err(EngineError::ambiguous(&written_name(e), "having clause"));
    }
    if let Some((_, windowed)) = found.first() {
        // A window function is computed over the finished result, so a clause
        // that decides what the result contains cannot read one even at second
        // hand: `SELECT b, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM t HAVING
        // rn > 2` is 3594 on 8.3. The value is right there in the projected
        // row, so without this the alias would have been answered with it.
        if *windowed {
            return Err(EngineError::window_alias_misplaced(&written_name(e)));
        }
        return Ok(());
    }

    // A column the query groups by, whether or not the select list also
    // returns it: `SELECT COUNT(*) FROM t GROUP BY s HAVING s > 'w'` answers
    // on 8.3.
    let rendered = e.to_string();
    if group_exprs.iter().any(|g| g.to_string() == rendered)
        || slot.is_some_and(|s| group_exprs.iter().any(|g| column_slot(refs, g) == Some(s)))
    {
        return Ok(());
    }

    // An enclosing query's column, which `refs` carries after this query's
    // own: MySQL resolves a HAVING name against outer queries as well, and
    // `SELECT MAX(x.b) FROM t x HAVING t.id > 0` inside a correlated subquery
    // answers on 8.3.
    if slot.is_some_and(|s| s >= own) {
        return Ok(());
    }

    Err(EngineError::unknown_column(
        &written_name(e),
        "having clause",
    ))
}

/// True when an expression calls a window function anywhere inside it.
fn contains_window(e: &Expr) -> bool {
    match e {
        Expr::Function(f) => f.over.is_some() || function_arguments(f).iter().any(contains_window),
        other => subexpressions(other).into_iter().any(contains_window),
    }
}

/// Rejects a selected expression that is neither aggregated nor grouped by.
///
/// Without this the engine evaluates it against the group's first row and
/// returns an arbitrary value, where MySQL under ONLY_FULL_GROUP_BY errors.
fn check_full_group_by(
    e: &Expr,
    top: &Expr,
    group_exprs: &[Expr],
    refs: &[ColumnRef],
    determined: &[usize],
) -> EngineResult<()> {
    // An expression built only from aggregates and constants is always fine.
    if !references_column(e) {
        return Ok(());
    }
    // So is one the query grouped by, compared as written.
    let rendered = e.to_string();
    if group_exprs.iter().any(|g| g.to_string() == rendered) {
        return Ok(());
    }
    // A column the grouping determines, whether or not it was named the same
    // way in the GROUP BY.
    if column_slot(refs, e).is_some_and(|slot| determined.contains(&slot)) {
        return Ok(());
    }

    // Into every part of it, because the grouped column can be anywhere:
    // `FLOOR(g)` is fine when `g` is grouped, and so is `UPPER(g)`. Recursing
    // through a hand-picked four variants refused both — and refused them
    // *only* once the variants above became visible to `references_column`,
    // which is what a list that has to be kept in step with another one does.
    match e {
        Expr::Function(f) if f.over.is_some() => return Ok(()),
        Expr::Function(f) if is_aggregate(&f.name.to_string()) => return Ok(()),
        Expr::Function(f) => {
            for argument in value_arguments(f) {
                check_full_group_by(&argument, top, group_exprs, refs, determined)?;
            }
            return Ok(());
        }
        other => {
            let inner = subexpressions(other);
            if !inner.is_empty() {
                for part in inner {
                    check_full_group_by(part, top, group_exprs, refs, determined)?;
                }
                return Ok(());
            }
        }
    }

    // The whole select item rather than the part of it that failed: the walk
    // descends to a leaf, and naming that leaf would report `x` for
    // `SUBSTRING(x, 1, 1)` — less than it knew before it started recursing.
    //
    // Two codes, not one. 8.3 answers 1055 only where there is a GROUP BY to
    // be absent from; an aggregated query with no GROUP BY at all is 1140,
    // ER_MIX_OF_GROUP_FUNC_AND_FIELDS_V2, and this engine sent 1055 for both.
    // `SELECT COUNT(*), b FROM t` and `SELECT b FROM t HAVING COUNT(*) > 0`
    // are the shapes that reach it, and a client branching on the code was
    // told the wrong thing about which rule it broke.
    //
    // Rendered from the tree, so a compound `EXTRACT` would name the call
    // `crate::rewrite` turned it into rather than the one the client wrote.
    let top = crate::rewrite::extract_as_written(&top.to_string());
    if group_exprs.is_empty() {
        return Err(EngineError::new(
            1140,
            "42000",
            format!(
                "In aggregated query without GROUP BY, expression '{top}' contains a \
                 nonaggregated column; this is incompatible with \
                 sql_mode=only_full_group_by"
            ),
        ));
    }
    Err(EngineError::new(
        1055,
        "42000",
        format!(
            "Expression '{top}' is not in GROUP BY clause and contains a nonaggregated \
             column; this is incompatible with sql_mode=only_full_group_by"
        ),
    ))
}

/// A function's arguments that are actually values.
///
/// `TIMESTAMPDIFF(DAY, a, b)` writes its unit as a bare keyword, which parses
/// as an identifier indistinguishable from a column. Anything walking the AST
/// for column references has to drop it, or `SELECT id, TIMESTAMPDIFF(DAY,
/// MIN(d), MAX(d)) ... GROUP BY id` is rejected for selecting a column named
/// `DAY` that does not exist.
fn value_arguments(f: &sqlparser::ast::Function) -> Vec<Expr> {
    let args = function_arguments(f);
    let name = f.name.to_string();
    if name.eq_ignore_ascii_case("timestampdiff") || name.eq_ignore_ascii_case("timestampadd") {
        return args.into_iter().skip(1).collect();
    }
    args
}

/// The column a `VALUES(col)` in an `ON DUPLICATE KEY UPDATE` names.
///
/// Inside that clause `VALUES(x)` does not call anything — it stands for the
/// value the insert would have written to `x`, which is how an upsert says
/// "take the new value".
fn values_reference(e: &Expr) -> Option<String> {
    let Expr::Function(f) = e else {
        return None;
    };
    if !f.name.to_string().eq_ignore_ascii_case("values") {
        return None;
    }
    match function_arguments(f).first() {
        Some(Expr::Identifier(ident)) => Some(ident.value.clone()),
        Some(Expr::CompoundIdentifier(parts)) => parts.last().map(|p| p.value.clone()),
        _ => None,
    }
}

/// The text of a string literal, as written without its quotes.
///
/// JSON_TABLE's paths are literals by grammar, so this cannot fail — but the
/// AST still carries a general value, and rendering that would keep the quotes.
fn literal_text(value: &sqlparser::ast::ValueWithSpan) -> String {
    match &value.value {
        sqlparser::ast::Value::SingleQuotedString(s)
        | sqlparser::ast::Value::DoubleQuotedString(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The sub-expressions an evaluator would read, for the walks that have to
/// look inside one.
///
/// One list rather than a `match` in each walk. Every one of those matches
/// ended in `_ => false`, so a variant missing from it was invisible rather
/// than an error: `SELECT TRIM(x) FROM t GROUP BY g` passed ONLY_FULL_GROUP_BY
/// and answered with an arbitrary row, and `SELECT CEIL(SUM(x))` reported
/// `sum()` as unsupported because the aggregate inside was never found. Both
/// walks had drifted behind `crate::expr::eval` by more than a dozen variants.
///
/// Anything `eval` can evaluate belongs here. Two deliberate omissions: a
/// `Function`, because the walks disagree about what an aggregate's arguments
/// mean, and a subquery, because its columns are resolved in its own scope
/// rather than this one.
pub(crate) fn subexpressions(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::BinaryOp { left, right, .. } => vec![left, right],
        Expr::UnaryOp { expr, .. }
        | Expr::Nested(expr)
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::IsTrue(expr)
        | Expr::IsFalse(expr)
        | Expr::IsNotTrue(expr)
        | Expr::IsNotFalse(expr)
        | Expr::IsUnknown(expr)
        | Expr::IsNotUnknown(expr)
        | Expr::Cast { expr, .. }
        | Expr::Collate { expr, .. }
        | Expr::Ceil { expr, .. }
        | Expr::Floor { expr, .. } => vec![expr],
        Expr::Prefixed { value, .. } => vec![value],
        Expr::Between {
            expr, low, high, ..
        } => vec![expr, low, high],
        Expr::InList { expr, list, .. } => std::iter::once(&**expr).chain(list.iter()).collect(),
        // The `expr` only: what the subquery selects is its own business.
        Expr::InSubquery { expr, .. } => vec![expr],
        Expr::Like { expr, pattern, .. }
        | Expr::ILike { expr, pattern, .. }
        | Expr::SimilarTo { expr, pattern, .. }
        | Expr::RLike { expr, pattern, .. } => vec![expr, pattern],
        Expr::Position { expr, r#in } => vec![expr, r#in],
        Expr::Extract { expr, .. } => vec![expr],
        Expr::Substring {
            expr,
            substring_from,
            substring_for,
            ..
        } => std::iter::once(&**expr)
            .chain(substring_from.as_deref())
            .chain(substring_for.as_deref())
            .collect(),
        Expr::Trim {
            expr,
            trim_what,
            trim_characters,
            ..
        } => std::iter::once(&**expr)
            .chain(trim_what.as_deref())
            .chain(trim_characters.iter().flatten())
            .collect(),
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => operand
            .as_deref()
            .into_iter()
            .chain(
                conditions
                    .iter()
                    .flat_map(|w| [&w.condition, &w.result].into_iter()),
            )
            .chain(else_result.as_deref())
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether an expression reads a column, for ONLY_FULL_GROUP_BY.
fn references_column(e: &Expr) -> bool {
    match e {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => true,
        // An aggregate consumes its columns, so they are not loose references.
        Expr::Function(f) if f.over.is_none() && is_aggregate(&f.name.to_string()) => false,
        Expr::Function(f) => value_arguments(f).iter().any(references_column),
        other => subexpressions(other).into_iter().any(references_column),
    }
}

/// Whether the query aggregates for a reason other than a GROUP BY: an
/// aggregate somewhere in the select list, or one written in the HAVING and
/// nowhere else.
///
/// One definition because two paths ask it and neither can be allowed to drift
/// from the other: the table-backed `select` ORs a GROUP BY on to decide
/// whether to route to `grouped`, and `select_without_table` asks it *without*
/// the GROUP BY, because there a GROUP BY is the difference between one row
/// and none. Measured on 8.3: `SELECT COUNT(*) WHERE 0` is one row and
/// `SELECT COUNT(*) WHERE 0 GROUP BY 'a'` is none.
///
/// An aggregate inside a subquery belongs to that subquery, which is what
/// `contains_aggregate` and `select_item_aggregates` both stop at.
fn aggregates_of_itself(select: &Select) -> bool {
    select.projection.iter().any(select_item_aggregates)
        || select.having.as_ref().is_some_and(contains_aggregate)
}

/// True when a select item is or contains an aggregate call.
pub(crate) fn select_item_aggregates(item: &SelectItem) -> bool {
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
        other => subexpressions(other).into_iter().any(contains_aggregate),
    }
}

/// The expression arguments of a function call.
pub(crate) fn function_arguments(f: &sqlparser::ast::Function) -> Vec<Expr> {
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
        "count" | "sum" | "avg" | "min" | "max" | "group_concat" | "json_arrayagg"
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

/// `text` cut to at most `limit` **bytes**, never through a character.
///
/// What `group_concat_max_len` measures is bytes, measured: at 4 a
/// `GROUP_CONCAT` of `'éé'` and `'ab'` is `'éé'`, four bytes and two
/// characters. But the cut lands on a character boundary — at 5 the answer for
/// `'ab'` then `'éé'` is `61622CC3A9`, and at 6 it is *still* that, because
/// the next character needs two bytes and only one is left. So this is a byte
/// budget spent a character at a time, not a byte offset.
fn truncate_bytes(mut text: String, limit: u64) -> String {
    if text.len() as u64 <= limit {
        return text;
    }
    // The end of the last character that fits whole. Every character is
    // measured, rather than the byte offset being clamped, because
    // `String::truncate` panics on an index inside one — and the answer would
    // be a broken character even if it did not.
    let mut end = 0usize;
    for (offset, ch) in text.char_indices() {
        if (offset + ch.len_utf8()) as u64 > limit {
            break;
        }
        end = offset + ch.len_utf8();
    }
    text.truncate(end);
    text
}

/// The `ORDER BY` an aggregate carries *inside its own parentheses*.
///
/// `GROUP_CONCAT(x ORDER BY y)` is the only spelling MySQL's grammar allows,
/// and it is not the window's `ORDER BY`: a window's belongs to the frame and
/// is reported under `window order by`, while this one is resolved as part of
/// the field list. Callers that mean both take `function_carried`.
fn own_order_by(f: &sqlparser::ast::Function) -> &[sqlparser::ast::OrderByExpr] {
    use sqlparser::ast::{FunctionArgumentClause, FunctionArguments};
    let FunctionArguments::List(list) = &f.args else {
        return &[];
    };
    list.clauses
        .iter()
        .find_map(|clause| match clause {
            FunctionArgumentClause::OrderBy(items) => Some(items.as_slice()),
            _ => None,
        })
        .unwrap_or(&[])
}

/// The separator `GROUP_CONCAT` was written with, or `,` for a call with no
/// `SEPARATOR` clause.
///
/// MySQL's grammar takes a *literal* here and nothing else — `SEPARATOR s`,
/// `SEPARATOR 1`, `SEPARATOR CONCAT('a')`, `SEPARATOR NULL`, `SEPARATOR
/// _utf8mb4'--'` and two adjacent literals are all 1064 on 8.3 — which is why
/// this reads a literal rather than evaluating an expression.
///
/// But the grammar's `text_string` is wider than a quoted string: a hex or bit
/// literal is a string of bytes and is taken. Measured on 8.3, `SEPARATOR
/// 0x2D`, `SEPARATOR X'2D'` and `SEPARATOR b'101101'` all join with `-`, and
/// `SEPARATOR 0x2D2D` with `--`. Reading the quoted forms and rendering
/// anything else through `Display` joined with the literal characters `X'2D'`
/// instead — the same shape of plausible wrong answer #0088 is about — so this
/// goes through `expr::literal`, which is the one definition of what a literal
/// means here.
///
/// That covers the hex forms and leaves the *bit* one refused with 1235, which
/// is deliberate: `expr::literal` has no value for a bit string, so `SELECT
/// b'101101'` is 1235 here as well. Giving the separator its own decoder would
/// make this one clause disagree with the rest of the engine about what
/// `b'101101'` is; #0141 is where that gap gets closed for all of them. A
/// refusal is the honest answer until then, and it is what the old code did
/// not do.
fn aggregate_separator(f: &sqlparser::ast::Function) -> EngineResult<String> {
    use sqlparser::ast::{FunctionArgumentClause, FunctionArguments};
    let FunctionArguments::List(list) = &f.args else {
        return Ok(",".to_owned());
    };
    for clause in &list.clauses {
        if let FunctionArgumentClause::Separator(literal) = clause {
            // The server default, and the collation is never read: only
            // the separator's *text* is taken, and a `GROUP_CONCAT`
            // separator is never compared against anything.
            return Ok(crate::expr::literal(
                &literal.value,
                crate::collation::Collation::default(),
            )?
            .as_text());
        }
    }
    Ok(",".to_owned())
}

/// How an aggregate call was written, beyond its name and the rows it folds.
///
/// `GROUP_CONCAT` is the only aggregate here that reads more of its own call
/// than one argument and a DISTINCT: it concatenates *every* argument per row,
/// orders the rows by its own `ORDER BY`, and joins them with its own
/// `SEPARATOR`. All three were parsed and thrown away — #0088 — so the answer
/// was a plausible string in the wrong order, which a caller comparing against
/// an expected value cannot tell from a wrong value.
struct AggregateCall {
    /// Every value argument, in written order. One for every aggregate here
    /// except `COUNT(*)`, which has none, and `GROUP_CONCAT`, which may have
    /// several.
    arguments: Vec<Expr>,
    distinct: bool,
    /// The call's own `ORDER BY`, with each position already resolved to the
    /// argument it names.
    order: Vec<SortKey>,
    separator: String,
}

impl AggregateCall {
    /// Reads one call, refusing an `ORDER BY` position no argument answers to.
    ///
    /// The position names an *argument* of the call, not an output column:
    /// `GROUP_CONCAT(s, n ORDER BY 2)` sorts by `n` on 8.3. Out of range is
    /// 1054 in `'order clause'` there, and so is `ORDER BY 0`.
    ///
    /// The same refusal is raised at resolve time by `refuse_at_resolve`, so a
    /// statement over an empty relation still fails; this one is what makes
    /// the fold total rather than something that has to trust that.
    fn read(f: &sqlparser::ast::Function) -> EngineResult<Self> {
        let arguments = value_arguments(f);
        let mut order = Vec::new();
        for item in own_order_by(f) {
            let expr = match written_position(&item.expr) {
                Some((position, written)) => arguments
                    .get(position.wrapping_sub(1))
                    .ok_or_else(|| EngineError::unknown_column(written, "order clause"))?
                    .clone(),
                None => item.expr.clone(),
            };
            order.push(SortKey {
                expr,
                descending: item.options.asc == Some(false),
            });
        }
        Ok(Self {
            arguments,
            distinct: aggregate_is_distinct(f),
            order,
            separator: aggregate_separator(f)?,
        })
    }

    /// The one argument every aggregate but `GROUP_CONCAT` folds over.
    fn argument(&self) -> Option<&Expr> {
        self.arguments.first()
    }
}

/// Every part of an assignment target, qualifier first.
fn assignment_parts(target: &AssignmentTarget) -> EngineResult<Vec<String>> {
    match target {
        AssignmentTarget::ColumnName(column) => Ok(name::parts(column)),
        other => Err(unsupported(format!("assignment target `{other}`"))),
    }
}

/// The name MySQL gives an unaliased select item.
///
/// For a computed column that is the *source text*, character for character:
/// `SELECT 1!=2` is named `1!=2` and `SELECT ( 1 + 2 ) * 3` keeps every space
/// it was written with. Rendering the parsed expression instead cannot be made
/// to match, because the same tree comes from spellings MySQL names
/// differently — `1!=2` and `1 <> 2` are one `NotEq`, and `doc->>'$.b'` and
/// `doc ->> '$.b'` are one `LongArrow`. A client keys a row by this name, so a
/// test reading the column back finds nothing.
///
/// A literal is the exception, and `literal_name` is where that lives. A column
/// reference is the other one, and `column_reference` is: both carry a name
/// from the moment they are parsed, so the text that spelled them — including
/// any parentheses and unary pluses around them — is never asked for.
fn render_expr(e: &Expr) -> String {
    match column_reference(e) {
        Some(Expr::Identifier(i)) => i.value.clone(),
        Some(Expr::CompoundIdentifier(parts)) => {
            parts.last().map(|p| p.value.clone()).unwrap_or_default()
        }
        // Capped only here. A column reference carries the name the catalog
        // holds, which is a different limit on a different path.
        //
        // Over the item as it was written rather than over the peeled one:
        // what is left of `+ABS(a)` after the peel is `ABS(a)`, and 8.3 names
        // that column `+ABS(a)`.
        _ => capped(
            literal_name(e)
                .or_else(|| source_text(e))
                .unwrap_or_else(|| e.to_string()),
        ),
    }
}

/// Refuses a view whose definition writes an alias longer than a column name
/// may be.
///
/// A view turns its aliases into stored column names, so the limit that
/// applies to a column applies to them — and 8.3 applies it to *every* alias
/// in the definition, not only to the ones that become the view's own columns.
/// Measured there: `CREATE VIEW v AS SELECT 1 AS x UNION SELECT 2 AS
/// <65 chars>` is 1166, and so is `... AS SELECT * FROM (SELECT 1 AS
/// <65 chars>) d`, where a plain `SELECT` over either answers.
///
/// The walk is its own rather than `collect_names`, because that one
/// follows the *leftmost* branch of a set operation, because pinning a name
/// into the others would rename nothing. Refusing has no such reason to stop.
///
/// A generated name is deliberately not checked. 8.3 does not refuse those —
/// it renames them, storing `SELECT 1+1+...` past 64 characters as
/// `Name_exp_1` — and refusing where it renames would be a new wrong answer in
/// place of the old one. That rename is not implemented here; see the note on
/// the empty name in `pin_in_body`, which is the same gap.
fn check_aliases_in_query(query: &Query) -> EngineResult<()> {
    if let Some(with) = query.with.as_ref() {
        for cte in &with.cte_tables {
            check_aliases_in_query(&cte.query)?;
        }
    }
    check_aliases_in_body(&query.body)
}

fn check_aliases_in_body(body: &SetExpr) -> EngineResult<()> {
    match body {
        SetExpr::Select(select) => {
            for item in &select.projection {
                if let SelectItem::ExprWithAlias { alias, expr } = item {
                    name::check_column_name(&alias.value)?;
                    check_aliases_in_expr(expr)?;
                } else if let SelectItem::UnnamedExpr(expr) = item {
                    check_aliases_in_expr(expr)?;
                }
            }
            for e in select.selection.iter().chain(select.having.iter()) {
                check_aliases_in_expr(e)?;
            }
            for table in &select.from {
                check_aliases_in_table(&table.relation)?;
                for join in &table.joins {
                    check_aliases_in_table(&join.relation)?;
                }
            }
            Ok(())
        }
        SetExpr::Query(inner) => check_aliases_in_query(inner),
        SetExpr::SetOperation { left, right, .. } => {
            check_aliases_in_body(left)?;
            check_aliases_in_body(right)
        }
        _ => Ok(()),
    }
}

/// Refuses an over-long alias written inside an expression's subquery.
///
/// `subexpressions` deliberately stops at a subquery and at a function's
/// arguments, because the walks that *resolve* names must: a subquery's
/// columns are resolved in its own scope, not the enclosing one. Refusing is
/// the opposite case. 8.3 applies the limit to every alias anywhere in a
/// view's definition, however deeply it is buried, so this walk descends where
/// that one stops. Measured there, all 1166, and all accepted here before:
///
/// ```text
/// CREATE VIEW v AS SELECT COALESCE((SELECT 1 AS <65>), 1) AS y
/// CREATE VIEW v AS SELECT 1 AS y FROM t WHERE i IN (SELECT 1 AS <65>)
/// CREATE VIEW v AS SELECT 1 AS y FROM t WHERE EXISTS (SELECT 1 AS <65>)
/// CREATE VIEW v AS SELECT 1 AS y FROM t HAVING MAX(i) = (SELECT 1 AS <65>)
/// CREATE VIEW v AS SELECT 1 AS y FROM t WHERE i > ALL (SELECT 1 AS <65>)
/// ```
///
/// while every one of those answers without the `CREATE VIEW`, which is what
/// makes this the view path's business and not the executor's.
///
/// `AnyOp` and `AllOp` are matched here rather than left to `subexpressions`
/// because that function does not carry them and ends in a catch-all, so the
/// `ALL` line above would walk into nothing. That catch-all is the same shape
/// the note on `subexpressions` records drifting before, and it is the
/// residual risk here too: a query-bearing variant this match does not name is
/// not refused rather than being a compile error.
fn check_aliases_in_expr(e: &Expr) -> EngineResult<()> {
    match e {
        Expr::Subquery(query) => check_aliases_in_query(query)?,
        Expr::InSubquery { subquery, .. } => check_aliases_in_query(subquery)?,
        Expr::Exists { subquery, .. } => check_aliases_in_query(subquery)?,
        Expr::AnyOp { left, right, .. } | Expr::AllOp { left, right, .. } => {
            check_aliases_in_expr(left)?;
            check_aliases_in_expr(right)?;
        }
        // A function's arguments, which `subexpressions` omits because the
        // resolving walks disagree about what an aggregate's arguments mean.
        // Nothing here reads the argument, only what it may hide.
        Expr::Function(f) => {
            for arg in function_arguments(f) {
                check_aliases_in_expr(&arg)?;
            }
        }
        _ => {}
    }
    for sub in subexpressions(e) {
        check_aliases_in_expr(sub)?;
    }
    Ok(())
}

fn check_aliases_in_table(factor: &TableFactor) -> EngineResult<()> {
    match factor {
        TableFactor::Derived { subquery, .. } => check_aliases_in_query(subquery),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            check_aliases_in_table(&table_with_joins.relation)?;
            for join in &table_with_joins.joins {
                check_aliases_in_table(&join.relation)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The text a view is stored as, with every unaliased column's name pinned to
/// it as an alias.
///
/// A view is kept as text and re-run on read, and the text is the parse tree
/// rendered back — which loses the spellings a column name is taken from.
/// sqlparser renders an adjacent literal run `'a' 'b'` as the one string it
/// denotes, so a view over it came back named `ab` where MySQL says `a`, and
/// the same expression written inline was named correctly. A view is the only
/// path where the name survives neither, which is the worst of the two.
///
/// MySQL stores its own views this way, measured on 8.3: `CREATE VIEW w AS
/// SELECT 'a' 'b'` is kept as ``select 'ab' AS `a` `` and `SELECT 1 MOD 3` as
/// ``select (1 % 3) AS `1 MOD 3` ``. Pinning is also what makes the name
/// survive `refresh_views` and the state image, both of which re-derive from
/// this text.
///
/// Guarded rather than trusted, and guarded on the *names* rather than on the
/// text. Rendering an identifier escapes it by a heuristic that cannot know
/// how the value was escaped to begin with: a name already holding a doubled
/// backtick is written back unchanged and reads as one backtick, and a name
/// holding a backslash renders as text that no longer parses at all. Only the
/// second of those is visible in the text, which is why the aliased query is
/// re-parsed and its names held against the ones it was built from. A view
/// that does not survive that keeps the text it would have had — a name from
/// the rendered tree is recoverable, a mangled one is not, and a definition
/// that will not parse is worse than either.
fn pinned_definition(query: &Query) -> String {
    let plain = query.to_string();
    let Some(pinned) = pin_column_names(query) else {
        return plain;
    };
    let text = pinned.to_string();
    let intended = pinned_names(&pinned);
    let survives = Parser::new(&MySqlDialect {})
        .try_with_sql(&text)
        .and_then(|mut p| p.parse_query())
        .is_ok_and(|reparsed| pinned_names(&reparsed) == intended);
    if survives {
        text
    } else {
        plain
    }
}

/// Every name the pinning wrote, in the order it walked them.
///
/// `None` for a select item that carries no alias, so a name that was pinned
/// and came back unpinned compares unequal rather than being skipped. The
/// walk is `pin_in_query`'s, position for position, which is the only reason
/// two of these can be compared at all.
fn pinned_names(query: &Query) -> Vec<Option<String>> {
    let mut out = Vec::new();
    collect_names(query, &mut out);
    out
}

fn collect_names(query: &Query, out: &mut Vec<Option<String>>) {
    if let Some(with) = query.with.as_ref() {
        for cte in &with.cte_tables {
            collect_names(&cte.query, out);
        }
    }
    collect_body_names(&query.body, out);
}

fn collect_body_names(body: &SetExpr, out: &mut Vec<Option<String>>) {
    match body {
        SetExpr::Select(select) => {
            for item in &select.projection {
                match item {
                    SelectItem::ExprWithAlias { alias, .. } => out.push(Some(alias.value.clone())),
                    SelectItem::UnnamedExpr(_) => out.push(None),
                    _ => {}
                }
            }
            for table in &select.from {
                collect_table_names(&table.relation, out);
                for join in &table.joins {
                    collect_table_names(&join.relation, out);
                }
            }
        }
        SetExpr::Query(inner) => collect_names(inner, out),
        SetExpr::SetOperation { left, .. } => collect_body_names(left, out),
        _ => {}
    }
}

fn collect_table_names(factor: &TableFactor, out: &mut Vec<Option<String>>) {
    match factor {
        TableFactor::Derived { subquery, .. } => collect_names(subquery, out),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            collect_table_names(&table_with_joins.relation, out);
            for join in &table_with_joins.joins {
                collect_table_names(&join.relation, out);
            }
        }
        _ => {}
    }
}

/// A copy of the query with each unaliased select item carrying its name.
///
/// `None` when there was nothing to pin, so the caller keeps the text it
/// already had rather than paying for a round trip that cannot change it.
fn pin_column_names(query: &Query) -> Option<Query> {
    let mut copy = query.clone();
    pin_in_query(&mut copy).then_some(copy)
}

/// Pins names everywhere they can be read back out.
///
/// A view's own columns come from the deciding select, but a derived table or
/// a CTE inside it names columns too and a `SELECT *` over one takes its names
/// from there — so a run buried in a subquery reaches the client just the
/// same. MySQL pins every select in a stored view for the same reason,
/// measured on 8.3: `CREATE VIEW w AS SELECT * FROM (SELECT 'a' 'b') d` is
/// kept as ``select `d`.`a` AS `a` from (select 'ab' AS `a`) `d` ``.
///
/// A subquery in an *expression* is deliberately not walked: its columns are
/// consumed by the expression around it and never named to a client.
fn pin_in_query(query: &mut Query) -> bool {
    let mut pinned = false;
    if let Some(with) = query.with.as_mut() {
        for cte in &mut with.cte_tables {
            pinned |= pin_in_query(&mut cte.query);
        }
    }
    // Not `||`: the body has to be walked whether or not a CTE was pinned.
    pinned | pin_in_body(&mut query.body)
}

fn pin_in_body(body: &mut SetExpr) -> bool {
    match body {
        SetExpr::Select(select) => {
            let mut pinned = false;
            for item in &mut select.projection {
                let SelectItem::UnnamedExpr(e) = item else {
                    continue;
                };
                let name = render_expr(e);
                // No identifier spells the empty name. MySQL calls that column
                // `Name_exp_1`, and inventing one here would be a second
                // divergence rather than a fix for this one.
                if name.is_empty() {
                    continue;
                }
                *item = SelectItem::ExprWithAlias {
                    expr: e.clone(),
                    alias: sqlparser::ast::Ident::with_quote('`', name),
                };
                pinned = true;
            }
            for table in &mut select.from {
                pinned |= pin_in_table(&mut table.relation);
                for join in &mut table.joins {
                    pinned |= pin_in_table(&mut join.relation);
                }
            }
            pinned
        }
        SetExpr::Query(inner) => pin_in_query(inner),
        // The leftmost branch of a set operation and nothing else: a union
        // takes its column names from its first branch, so aliasing the others
        // would rename nothing and change the text of every view over one.
        SetExpr::SetOperation { left, .. } => pin_in_body(left),
        _ => false,
    }
}

fn pin_in_table(factor: &mut TableFactor) -> bool {
    match factor {
        TableFactor::Derived { subquery, .. } => pin_in_query(subquery),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            let mut pinned = pin_in_table(&mut table_with_joins.relation);
            for join in &mut table_with_joins.joins {
                pinned |= pin_in_table(&mut join.relation);
            }
            pinned
        }
        _ => false,
    }
}

/// The longest generated column name MySQL will report, in bytes.
///
/// Measured on 8.3: `SELECT '<256 a>'` and `SELECT CONCAT('<256 a>')` both
/// come back with a 255-byte name, and 128 two-byte characters come back as
/// 127 of them rather than a half character, so the cut is on a boundary. An
/// *alias* is an identifier and is cut at 256 instead, which is a different
/// path and is not this.
const MAX_GENERATED_NAME: usize = 255;

/// Cuts a generated name to what MySQL would report.
fn capped(mut name: String) -> String {
    if name.len() > MAX_GENERATED_NAME {
        let mut at = MAX_GENERATED_NAME;
        while !name.is_char_boundary(at) {
            at -= 1;
        }
        name.truncate(at);
    }
    name
}

/// What is left of an item once the two constructs that hand their operand
/// straight back are discarded: parentheses and unary plus.
///
/// MySQL's parser really does drop them rather than build a node for them —
/// `'+' simple_expr` is its operand — which is why they are invisible in
/// everything downstream of the parse, from the name to the width to the table
/// the column reports. Unary minus builds a node and is not peeled here.
fn peeled(e: &Expr) -> &Expr {
    let mut inner = e;
    loop {
        inner = match inner {
            Expr::Nested(nested) => nested,
            Expr::UnaryOp {
                op: sqlparser::ast::UnaryOperator::Plus,
                expr,
            } => expr,
            _ => return inner,
        };
    }
}

/// The column reference an item is, seen through parentheses and unary plus.
///
/// `SELECT +a`, `SELECT (a)`, `SELECT +(a)` and `SELECT + +a` are the column
/// `a` on 8.3 in *every* field of the definition — name, type, width, decimals,
/// flags, and the schema and table it reports — and not a computed column that
/// happens to answer the same value. `CREATE TABLE c AS SELECT +a FROM t` copies
/// `decimal(10,2) NOT NULL` the same way `SELECT a` does, and a view over it is
/// stored as ``select `t`.`a` AS `a` `` with the plus gone.
///
/// The peel chooses which item this is, never what it is named: with anything
/// else underneath the item is the whole written text again, so `SELECT +ABS(a)`
/// is `+ABS(a)` and `SELECT -a` is `-a`, both computed.
///
/// A variable parses as an identifier and is not one of these, measured both
/// ways on 8.3: `SELECT +@v` is named `+@v` and `SELECT (@@sql_mode)` keeps its
/// parentheses, because a variable carries no parse-time name for the peel to
/// find. Quoting is what separates the two, and it has to be read: a plus in
/// front of a backtick-quoted `@v` is a column of that name, and 8.3 names that
/// one `@v`.
fn column_reference(e: &Expr) -> Option<&Expr> {
    let inner = peeled(e);
    let variable = match inner {
        Expr::Identifier(i) => is_variable(i),
        Expr::CompoundIdentifier(parts) => parts.first().is_some_and(is_variable),
        _ => return None,
    };
    (!variable).then_some(inner)
}

/// Whether an identifier names a variable rather than a column.
///
/// The quoting is half the answer: `@v` is a user variable and `` `@v` `` is a
/// column that happens to be called that, and MySQL reads the two differently.
fn is_variable(i: &sqlparser::ast::Ident) -> bool {
    i.quote_style.is_none() && i.value.starts_with('@')
}

/// The name MySQL's parser attaches to a literal, if this item is one.
///
/// MySQL names most unaliased items after their source text, but a literal is
/// named as it is read and carries that name from then on — so a string is
/// named by its *contents* rather than by the text that spelled them:
/// `SELECT 'abc'` is `abc`, `SELECT 'it''s'` is `it's`, and `SELECT 'a\nb'` is
/// named with a real newline in it.
///
/// The name survives the two constructs that hand an item straight back,
/// parentheses and unary plus, which is why they are peeled here: `SELECT
/// ('abc')`, `SELECT +'abc'` and `SELECT + +'abc'` are all `abc`, and `SELECT
/// +1` is `1`. Peeling only chooses *which* item to ask, never the name —
/// anything that is not a literal underneath still gets the whole written
/// text, so `SELECT +(1+2)` is `+(1+2)` and `SELECT +TRUE` is `+TRUE`. All
/// measured on 8.3.
///
/// Unary minus is not peeled, and is not an oversight: `SELECT -1` is `-1` but
/// `SELECT - 1` is `- 1`, which is the source text and not the literal.
fn literal_name(e: &Expr) -> Option<String> {
    let Expr::Value(value) = peeled(e) else {
        return None;
    };
    // A bound parameter reaches the parser as the literal it was bound to, and
    // would otherwise be named by that literal's contents: `SELECT ?` bound to
    // `'hello world'` was named `hello world` and described `NULL`, because a
    // describe has no value to splice. MySQL names it `?` at prepare and `?`
    // at execute, whatever is bound — measured through `COM_STMT_PREPARE` and
    // `COM_STMT_EXECUTE`, along with `SELECT ?+1` naming `?+1` and `SELECT
    // CONCAT(?,?)` naming `CONCAT(?,?)`, spacing and all. Those two come out
    // of `source_text` on their own once the binder records its edits; this is
    // the one shape that never reaches it, because a literal is named where it
    // is read and never asks for the text that spelled it. #0109.
    if is_bound_parameter(&value.span) {
        return Some("?".to_owned());
    }
    use sqlparser::ast::Value as V;
    match &value.value {
        V::SingleQuotedString(s) | V::DoubleQuotedString(s) | V::NationalStringLiteral(s) => Some(
            string_literal_name(first_adjacent_literal(value, s).as_deref().unwrap_or(s)),
        ),
        // The token as it was written, not the value it denotes: `SELECT 1e3`
        // is named `1e3` and `SELECT 1E3` is named `1E3`.
        V::Number(n, _) => Some(n.to_string()),
        // The one literal MySQL canonicalises: `SELECT NuLl` is named `NULL`.
        V::Null => Some("NULL".to_owned()),
        // Everything else is named by its source text, `TRUE`, `X'41'`, `b'1'`
        // and `0x41` included — none of them carry a parse-time name, which is
        // why `SELECT +TRUE` keeps its plus where `SELECT +1` does not.
        _ => None,
    }
}

/// Whether the literal at `span` is a bound parameter the binder wrote there.
///
/// Answered through the mapping, which is the only thing that still knows: by
/// the time this runs the placeholder has been replaced by the value's literal
/// text and the two are indistinguishable in the tree — a bound `'?'` and a
/// written `'?'` parse to the same node.
///
/// The statement is asked first, and it is what keeps this off the hot path:
/// converting a span to a byte offset walks the text, twice, and a query with
/// a hundred literals in its select list would pay for it a hundred times.
/// Nearly every statement has no parameters at all, and one that has none
/// cannot have a span that maps back to a `?`.
///
/// `false` outside a statement, where nothing set `SOURCE`, and `false` for a
/// span the mapping cannot place — the caller then names the literal the way
/// it did before this existed, which is the answer for every literal that is
/// not a parameter anyway.
fn is_bound_parameter(span: &sqlparser::tokenizer::Span) -> bool {
    let Some(origin) = SOURCE.with(|s| s.borrow().as_ref().map(|c| std::rc::Rc::clone(&c.source)))
    else {
        return false;
    };
    if !origin.has_parameters() {
        return false;
    }
    let Some(start) = rewrite::byte_offset(&origin.sql, span.start) else {
        return false;
    };
    let Some(end) = rewrite::byte_offset(&origin.sql, span.end) else {
        return false;
    };
    origin.as_written(start..end) == Some("?")
}

/// A string literal's contents as MySQL would name a column with them.
///
/// Leading characters that are not printable ASCII are dropped, trailing ones
/// are kept: `SELECT '  a'` is named `a`, `SELECT 'a  '` is named `a  `, and
/// `SELECT '   '` has an empty name. Only ASCII, because MySQL classifies by a
/// character-set ctype table that stops at 128 — a leading no-break space
/// survives into the name.
fn string_literal_name(contents: &str) -> String {
    contents
        .trim_start_matches(|c: char| c.is_ascii() && !c.is_ascii_graphic())
        .to_owned()
}

/// The contents of the *first* literal in an adjacent-literal concatenation.
///
/// `SELECT 'a' 'b' 'c'` is one string `abc` in MySQL and is named `a`: the
/// name is fixed when the first literal is read and the rest are appended to
/// the value afterwards. sqlparser joins them into a single `Value` and the
/// boundary is gone from the tree, but the value's span still covers only the
/// first token, so the token is read back out of the statement text.
///
/// `None` when the text or the offsets are not there, and the caller falls
/// back to `joined` — which is the same string whenever there was only one
/// literal, and that is every statement but this one.
///
/// Which is also why it settles the common case without tokenizing anything.
/// A span holding the joined value between two quotes and nothing else cannot
/// be the first of a run: a run makes the joined value *longer* than the first
/// token while the span still covers only that token, so the two lengths part
/// company. The comparison is against the decoded value rather than only its
/// length, because a length can be made to match — `'a''a' 'b'` has one byte
/// of quote doubling in the first token and one byte of `'b'` in the joined
/// value, so it balances and the interiors are what separate them.
fn first_adjacent_literal(value: &sqlparser::ast::ValueWithSpan, joined: &str) -> Option<String> {
    let sql = SOURCE.with(|s| s.borrow().as_ref().map(|c| std::rc::Rc::clone(&c.source)))?;
    let sql = &sql.sql;
    let start = rewrite::byte_offset(sql, value.span.start)?;
    let end = rewrite::byte_offset(sql, value.span.end)?;
    let first = sql.get(start..end)?;
    if first.len() == joined.len() + 2 && first.get(1..first.len() - 1) == Some(joined) {
        return None;
    }
    // Tokenized rather than unquoted by hand: which backslash escapes and
    // which doubled quotes a literal decodes is the tokenizer's answer, and a
    // second copy of it here would only be a second copy to get wrong.
    let tokens = sqlparser::tokenizer::Tokenizer::new(&MySqlDialect {}, first)
        .tokenize()
        .ok()?;
    match tokens.first()? {
        sqlparser::tokenizer::Token::SingleQuotedString(s)
        | sqlparser::tokenizer::Token::DoubleQuotedString(s)
        | sqlparser::tokenizer::Token::NationalStringLiteral(s) => Some(s.clone()),
        _ => None,
    }
}

/// How far either side of its reported span an expression's text is looked for.
///
/// sqlparser reports a span narrower than the text on both sides, sometimes by
/// a lot: `COUNT(*)` covers five of its eight characters, `( 1 + 2 ) * 3`
/// starts at the `1`, and `SUBSTRING('abc',2)` starts at the `'abc'` because
/// the parser gives it its own grammar and spans only the arguments. What is
/// missing is a call's name and its delimiters, so this is a backstop against
/// scanning a long statement when nothing will ever match.
const SPAN_SLACK: usize = 64;

/// The text an expression was written as, if it can be recovered exactly.
///
/// `None` rather than a guess: the caller falls back to rendering the tree,
/// which is wrong about spacing but never about *which* expression it names.
///
/// Two texts are in play and each answers a different question. The search
/// runs over the *rewritten* statement, because that is what the spans point
/// into and what can be parsed back to check a candidate — the original may
/// not even parse without the mode that produced it, `SELECT "x" + 1` under
/// ANSI_QUOTES being exactly that. The answer is then read out of the text the
/// client sent, through the mapping, because that is the name MySQL gives.
fn source_text(e: &Expr) -> Option<String> {
    let origin = SOURCE.with(|s| s.borrow().as_ref().map(|c| std::rc::Rc::clone(&c.source)))?;
    let sql = &origin.sql;
    let span = e.span();
    let reported = rewrite::byte_offset(sql, span.start)?;
    let from = rewrite::byte_offset(sql, span.end)
        .unwrap_or(reported)
        .max(reported);
    if let Some(known) = SOURCE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|c| c.names.borrow().get(&(reported, from)).cloned())
    }) {
        return known;
    }
    let rendered = e.to_string();
    let limit = (from + SPAN_SLACK).min(sql.len());

    // Outward from the reported span, nearest first, so the answer is the
    // tightest slice that parses back to this same expression. Anything longer
    // would have to denote something else, and a comma or a clause keyword
    // ends the run on its own by failing to parse.
    //
    // Backwards from the reported start, but only as far as the select item
    // itself: a comma, a semicolon or a quote before it belongs to something
    // else and ends the walk. Within that, only positions a token could begin
    // at are tried, which is what keeps this to a handful of candidates rather
    // than one per character.
    let floor = reported.saturating_sub(SPAN_SLACK);
    let starts = (floor..=reported)
        .rev()
        .take_while(|start| {
            sql.get(*start..reported)
                .is_some_and(|skipped| !skipped.contains([',', ';', '\'', '"', '`']))
        })
        .filter(|start| *start == reported || starts_a_token(&origin, *start));

    let mut answer = None;
    for start in starts {
        if !sql.is_char_boundary(start) {
            continue;
        }
        let found = ends(sql, start, from, limit)
            .into_iter()
            .map(|end| start + sql[start..end].trim_end().len())
            .find(|end| *end > start && parses_back_to(&sql[start..*end], &rendered));
        if let Some(end) = found {
            // The name is the range as the client wrote it, which is the same
            // text whenever no pass touched it and the point of the mapping
            // when one did. Empty only if the whole of it was inserted by the
            // rewriter, which no select item can be — and the caller's
            // fallback is a better answer than a column with no name.
            answer = origin
                .as_written(start..end)
                .filter(|written| !written.is_empty())
                .map(str::to_owned);
            break;
        }
    }
    // Remembered either way: a failure costs the whole grid, and asking again
    // would cost it again.
    SOURCE.with(|s| {
        if let Some(current) = s.borrow().as_ref() {
            current
                .names
                .borrow_mut()
                .insert((reported, from), answer.clone());
        }
    });
    answer
}

/// Where an expression's text could end, nearest first.
///
/// Stops at the comma that ends the select item and at the parenthesis that
/// closes the one enclosing it, so a slice is never grown into the next item
/// or past the list. Without that the search runs its whole budget every time
/// nothing matches, on every prepare and every execute.
///
/// Counted from `start` rather than from `from`, because `from` is usually
/// *inside* the expression — the reported end of `CONCAT('a','b')` falls
/// between the parentheses, and a depth starting there reads the closing one
/// as the end of an enclosing group.
///
/// By characters, not bytes: stepping a byte at a time indexed into the middle
/// of a multi-byte one, and `SELECT 'café'` aborted the process.
fn ends(sql: &str, start: usize, from: usize, limit: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut characters = sql[start..limit].chars();
    let mut at = start;
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    loop {
        if at >= from {
            out.push(at);
        }
        let Some(c) = characters.next() else { break };
        match quote {
            // A backslash hides the next character, so `'a\'b'` is one string
            // and not two. Getting this wrong only costs a fallback to
            // rendering, but it costs it on the literals most likely to hold
            // punctuation.
            Some(_) if escaped => escaped = false,
            Some(_) if c == '\\' => escaped = true,
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' | '`' => quote = Some(c),
                '(' => depth += 1,
                // The delimiter that ends the item: this position is already
                // recorded, so the slice stops just before it.
                ')' if depth == 0 => break,
                ')' => depth -= 1,
                ',' if depth == 0 => break,
                _ => {}
            },
        }
        at += c.len_utf8();
    }
    out
}

/// Whether `slice` is a complete expression rendering identically to `rendered`.
///
/// Compared through `Display` on both sides so the spans differ harmlessly; a
/// structural comparison would never match, because the re-parsed copy carries
/// offsets into a different string.
fn parses_back_to(slice: &str, rendered: &str) -> bool {
    let Ok(mut parser) = Parser::new(&MySqlDialect {}).try_with_sql(slice) else {
        return false;
    };
    let Ok(parsed) = parser.parse_expr() else {
        return false;
    };
    // The whole slice, not a prefix of it: `COUNT(*` stops early and would
    // otherwise pass for `COUNT`.
    parser.peek_token().token == sqlparser::tokenizer::Token::EOF && parsed.to_string() == rendered
}

/// Whether a token could begin at this offset.
///
/// A name runs together with what precedes it or it is not the same name, so
/// only a boundary is worth trying: `SUBSTRING(` is reached in one step from
/// the space before it rather than nine.
///
/// Asked of the text the client sent as well as of the rewrite, and a boundary
/// in either is enough. A pass that closes a gap makes a junction that was
/// never written: under IGNORE_SPACE the parser is handed `SELECT(COUNT(*))`,
/// so the `(` the name has to start at follows the `T` of `SELECT` — and the
/// walk refused to start there, leaving `SELECT (COUNT (*))` named by the
/// rendered tree. It fired only on a parenthesised item written *first*,
/// because anywhere else the character before the `(` is a comma or an
/// operator and no gap is closed at all.
///
/// The other direction costs nothing. A pass that inserts text makes a
/// boundary the original does not have, and the extra candidate is still held
/// to parsing back to the same expression before it can be an answer.
fn starts_a_token(source: &rewrite::Normalised, at: usize) -> bool {
    let sql = &source.sql;
    if !sql.is_char_boundary(at) {
        return false;
    }
    let a_boundary = |text: &str| {
        text.chars()
            .next_back()
            .is_none_or(|previous| !previous.is_alphanumeric() && previous != '_')
    };
    a_boundary(&sql[..at]) || source.written_before(at).is_some_and(a_boundary)
}

/// What the current parse tree came from, for the parts of it that the tree
/// alone does not say.
struct Origin {
    /// The text the parser was given, the text the client sent, and how the
    /// two line up — so a computed column can be named after what was written
    /// even where the rewriter changed it.
    source: std::rc::Rc<rewrite::Normalised>,
    /// What each span has already been found to be called.
    ///
    /// A name is asked for at least twice per column — once to plan the
    /// projection and once to describe it — and a miss costs a grid of parse
    /// attempts rather than one.
    names: std::cell::RefCell<std::collections::HashMap<(usize, usize), Option<String>>>,
    /// Whether `||` concatenates. It changes how tightly the operator binds
    /// and therefore what `DIV` beside it divides, which is not recoverable
    /// from the tree.
    pipes_as_concat: bool,
    /// Whether `REAL` names four bytes rather than eight. The tree records
    /// only that the word was written; which type it declares is the mode's
    /// answer, not the parser's.
    real_as_float: bool,
    /// Whether `-` stays signed over an UNSIGNED operand. The tree records the
    /// operator; which domain it works in is the mode's answer.
    no_unsigned_subtraction: bool,
    /// Whether a write that would truncate or clamp a value fails instead.
    strict: bool,
    /// How many extra places a quotient and an average are computed to. Not a
    /// mode but a session variable, read here for the same reason the modes
    /// are: it decides the *scale a column is described with* — `a / b` over a
    /// `DECIMAL(10,2)` shows six places at the default and two at 0 — and the
    /// describing happens several recursions below the only place that holds
    /// the session.
    div_precision_increment: usize,
}

thread_local! {
    /// The statement being parsed or run on this thread.
    ///
    /// A thread-local for the same reason as `SHAPING` below: this is needed
    /// deep inside projection and expression handling, several recursions down
    /// from the only place that has it, and threading it through would put the
    /// same argument in signatures that do not otherwise care. A statement
    /// runs to completion on one thread, so this is exactly its scope.
    static SOURCE: std::cell::RefCell<Option<Origin>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether `||` concatenates in the statement being handled.
///
/// `false` without one, which is MySQL's default: `||` is OR unless
/// `PIPES_AS_CONCAT` says otherwise.
pub(crate) fn pipes_as_concat() -> bool {
    SOURCE.with(|s| {
        s.borrow()
            .as_ref()
            .is_some_and(|current| current.pipes_as_concat)
    })
}

/// Whether `REAL` means `FLOAT` in the statement being handled.
///
/// Read here rather than passed to `SqlType::from_ast` for the same reason as
/// above: the type is built from a column definition and from a cast, both of
/// them several calls below the only place that holds the session, and a
/// `mode` argument would have to be added to signatures that do not otherwise
/// care — column building, cast nullability, computed column types.
///
/// `false` without a statement in scope, which is MySQL's default: `REAL` is
/// `DOUBLE` unless `REAL_AS_FLOAT` says otherwise.
pub(crate) fn real_as_float() -> bool {
    SOURCE.with(|s| {
        s.borrow()
            .as_ref()
            .is_some_and(|current| current.real_as_float)
    })
}

/// How many extra places a quotient and an average carry in the statement
/// being handled.
///
/// Read here rather than passed to the walk that describes a column for the
/// reason the modes above are: `computed_column_type` and the shape walk
/// beside it run several recursions below the only place that holds the
/// session, through paths — a derived table, a view, a `DESCRIBE` — that have
/// no session argument at all.
///
/// The documented default without a statement in scope, which is what the
/// arithmetic itself falls back to.
pub(crate) fn div_precision_increment() -> usize {
    SOURCE.with(|s| {
        s.borrow()
            .as_ref()
            .map_or(crate::decimal::DEFAULT_DIV_PRECISION_INCREMENT, |current| {
                current.div_precision_increment
            })
    })
}

/// Whether a write that would lose data fails in the statement being handled.
///
/// Read here rather than passed to `SqlType::store` for the same reason as
/// above: a value is written into a column several calls below the only place
/// that holds the session, through paths — a DDL default, a generated column,
/// an `AUTO_INCREMENT` — that have no session argument at all.
///
/// `true` without a statement in scope, which is MySQL's default: the shipped
/// `sql_mode` carries `STRICT_TRANS_TABLES`, so silently cutting a value down
/// is the behaviour that has to be asked for. The other flags read this way
/// default the other direction because their MySQL default is off.
pub(crate) fn strict() -> bool {
    SOURCE.with(|s| s.borrow().as_ref().is_none_or(|current| current.strict))
}

/// Whether `-` is signed over an UNSIGNED operand in the statement being run.
///
/// Read here rather than passed to `expr::arithmetic` for the same reason as
/// above: a subtraction is evaluated several recursions below the only place
/// that holds the session, and a `mode` argument would have to be threaded
/// through every operator that does not care.
///
/// `false` without a statement in scope, which is MySQL's default: an unsigned
/// operand makes a subtraction unsigned unless `NO_UNSIGNED_SUBTRACTION` says
/// otherwise.
pub(crate) fn no_unsigned_subtraction() -> bool {
    SOURCE.with(|s| {
        s.borrow()
            .as_ref()
            .is_some_and(|current| current.no_unsigned_subtraction)
    })
}

/// Publishes the statement text for the duration of a scope.
///
/// Restored through a guard rather than a trailing assignment: a stale text
/// left behind would name columns after whatever the *previous* statement said
/// at those offsets, which is a wrong answer that reads like a real one.
struct Source(Option<Origin>);

impl Source {
    fn set(normalised: &std::rc::Rc<rewrite::Normalised>, session: &Session) -> Self {
        let mode = session.sql_mode();
        let current = Origin {
            source: std::rc::Rc::clone(normalised),
            names: std::cell::RefCell::new(std::collections::HashMap::new()),
            pipes_as_concat: mode.pipes_as_concat,
            real_as_float: mode.real_as_float,
            no_unsigned_subtraction: mode.no_unsigned_subtraction,
            strict: mode.strict,
            div_precision_increment: session.div_precision_increment(),
        };
        Self(SOURCE.with(|s| s.replace(Some(current))))
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        let previous = self.0.take();
        SOURCE.with(|s| *s.borrow_mut() = previous);
    }
}

/// The stored table behind the name a relation calls one of its sources.
///
/// `RelColumn::table` is the alias where the FROM clause gave one, and a
/// default has to be read out of the table that alias stands for — reading it
/// out of a *table* of that name is how `SELECT Other2.name FROM Src Other2`
/// copied the wrong default. `sources` and `base_tables` are parallel, which
/// is what makes the translation a lookup rather than a guess.
fn base_table<'a>(relation: &'a Relation, alias: &str) -> Option<&'a str> {
    relation
        .source_index(alias)
        .and_then(|i| relation.base_tables.get(i))
        .map(String::as_str)
}

fn wire_type(value: &Value) -> ColumnType {
    match value {
        Value::Int(_) => ColumnType::LongLong,
        // An integer like the arm above it, and *not* the VAR_STRING the
        // catch-all gave it: `to_wire` sends a `WireValue::U64`, so a column
        // announced VAR_STRING put eight raw bytes where a binary client was
        // reading a length-encoded string and desynchronised the row. MySQL
        // types `VALUES ROW(18446744073709551615)` LONGLONG UNSIGNED.
        Value::Unsigned(_) => ColumnType::LongLong,
        Value::Float(_) => ColumnType::Double,
        Value::Bytes(_) => ColumnType::Blob,
        // A document is declared as a string, like every other computed
        // expression here: a JSON column keeps its declared JSON type, but
        // singling out a *computed* document would make it the one expression
        // whose type is known when dates and decimals are not.
        _ => ColumnType::VarString,
    }
}

/// The answer to one of the diagnostics statements.
///
/// Column metadata measured on 8.3 rather than invented: `Level` and `Message`
/// are VAR_STRING, `Code` is an **unsigned** LONG, and all three are NOT NULL,
/// while the `SHOW COUNT(*)` forms answer a nullable unsigned LONGLONG named
/// after the variable they stand for — `@@session.warning_count`. A client
/// that reads `Code` as a signed 32-bit value still gets the right number, but
/// the flags are what an ORM maps the column with, and #0111 is what happens
/// when they are guessed.
fn show_diagnostics(session: &Session, show: &ShowDiagnostics) -> ResultSet {
    let area = session.diagnostics();

    if show.count_only {
        let (name, count) = match show.errors_only {
            true => ("@@session.error_count", area.error_count()),
            false => ("@@session.warning_count", area.count()),
        };
        let mut result = ResultSet::scalar(name, ColumnType::LongLong, Value::Int(count as i64));
        result.columns[0].unsigned = true;
        return result;
    }

    let mut level = OutColumn::new("Level", ColumnType::VarString);
    level.nullable = false;
    let mut code = OutColumn::new("Code", ColumnType::Long);
    code.nullable = false;
    code.unsigned = true;
    let mut message = OutColumn::new("Message", ColumnType::VarString);
    message.nullable = false;

    // Filtered before it is limited, so `SHOW ERRORS LIMIT 1` is the first
    // error and not the first condition if that happens to be one.
    let shown: Vec<&crate::diagnostics::Condition> = match show.errors_only {
        true => area.errors().collect(),
        false => area.conditions().iter().collect(),
    };
    // `LIMIT offset, count`, with 0 for an absent offset. Saturating rather
    // than indexing: an offset past the end is an empty answer on 8.3, not a
    // panic here.
    let shown = match show.limit {
        Some((offset, count)) => shown
            .into_iter()
            .skip(offset as usize)
            .take(count as usize)
            .collect(),
        None => shown,
    };

    ResultSet {
        columns: vec![level, code, message],
        rows: shown
            .into_iter()
            .map(|condition| {
                vec![
                    Value::text(condition.level.as_str().to_owned()),
                    Value::Int(i64::from(condition.code)),
                    Value::text(condition.message.clone()),
                ]
            })
            .collect(),
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

/// The rows of a table-shaped result, as a temporary table would hold them.
///
/// A derived table and a CTE both give an expression a *column* to live in,
/// and two things about the value stop at that boundary.
///
/// MySQL stops calling it a boolean: measured on 8.3, `SELECT JSON_ARRAY(x)
/// FROM (SELECT TRUE AS x) t` is `[1]` where the bare `SELECT
/// JSON_ARRAY(TRUE)` is `[true]`, and a CTE answers the same.
///
/// And a decimal loses the places it carries past the ones it is described
/// with, because the field it is written into has only the described ones:
/// `SELECT JSON_ARRAY(x) FROM (SELECT 1/3 AS x) t` is `[0.3333]` there where
/// the same expression unmaterialised is `[0.333333333]`, and `SUM` over that
/// derived table totals the four-place value.
///
/// A *scalar* subquery is neither of these and keeps both — `(SELECT TRUE) =
/// CAST('true' AS JSON)` is 1 there and `JSON_ARRAY((SELECT 1/3))` is
/// `[0.333333333]` — which is the opposite of the rule an ENUM follows, so
/// those are stripped in different places.
fn materialised(rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    rows.into_iter()
        .map(|row| {
            row.into_iter()
                .map(|value| value.untag_bool().settled())
                .collect()
        })
        .collect()
}
