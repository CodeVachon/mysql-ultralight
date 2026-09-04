//! Adapts the engine to the server's `Handler` trait.
//!
//! The engine knows nothing about packets and the server knows nothing about
//! SQL; this is the only place the two meet.

use std::sync::Arc;

use mysql_ultralight_engine::value::Value as EngineValue;
use mysql_ultralight_engine::{Engine, Outcome, ServerConfig, Session};
use mysql_ultralight_protocol::{Capabilities, ColumnDef, ErrPacket, OkPacket, Value as WireValue};
use mysql_ultralight_server::{Answered, Handler, HandlerResult, Prepared, Response, ResultSet};

pub struct EngineHandler {
    engine: Arc<Engine>,
    session: Session,
}

impl EngineHandler {
    pub fn new(engine: Arc<Engine>, config: ServerConfig, connection_id: u32) -> Self {
        Self {
            engine,
            session: Session::new(config, connection_id),
        }
    }

    /// Runs one statement and shapes the outcome for the wire.
    fn run(&mut self, sql: &str, parameters: &[EngineValue]) -> HandlerResult {
        let outcome = self
            .engine
            .execute(&mut self.session, sql, parameters)
            .map_err(ErrPacket::from)?;
        // The statement has just settled the diagnostics area, so this is its
        // own count and not the previous statement's.
        let warnings = self.session.diagnostics().wire_count();
        Ok(Self::shape(outcome, warnings))
    }

    /// The columns a statement will return, as the wire declares them.
    ///
    /// Worked out at prepare time because the client caches it: a prepare that
    /// declares no columns leaves every later execution nameless. A statement
    /// whose shape cannot be established without running something that might
    /// write declares nothing, which is where this started but is at least not
    /// a wrong answer.
    ///
    /// An `Err` is the statement being refused rather than its shape being
    /// unknown — a column reference nothing in scope answers to — and MySQL
    /// refuses it at prepare time too.
    fn declared_columns(&self, sql: &str, param_count: u16) -> Result<Vec<ColumnDef>, ErrPacket> {
        match self
            .engine
            .describe(&self.session, sql, param_count as usize)
        {
            Ok(described) => Ok(described
                .map(|columns| columns.iter().map(|c| c.to_wire()).collect())
                .unwrap_or_default()),
            Err(e) => Err(ErrPacket::from(e)),
        }
    }

    /// Turns one engine outcome into the response the wire carries.
    ///
    /// `warnings` is how many conditions the statement raised. Both shapes
    /// carry it: an OK packet has a field for it, and a result set puts it in
    /// the packet that terminates it -- a statement returning rows has no
    /// other way to report one, so a hard-coded 0 there is a client told a
    /// clean success where MySQL reports a problem. #0204.
    fn shape(outcome: Outcome, warnings: u16) -> Response {
        match outcome {
            Outcome::Rows(rows) => {
                let columns = rows.columns.iter().map(|c| c.to_wire()).collect();
                let wire_rows = rows
                    .rows
                    .into_iter()
                    .map(|row| row.iter().map(EngineValue::to_wire).collect())
                    .collect();
                Response::ResultSet(
                    ResultSet::with_rows(columns, wire_rows).warning_count(warnings),
                )
            }
            Outcome::Affected {
                rows,
                last_insert_id,
            } => Response::Ok(OkPacket {
                warnings,
                ..OkPacket::new(rows, last_insert_id)
            }),
        }
    }
}

impl Handler for EngineHandler {
    fn negotiated(&mut self, capabilities: Capabilities) {
        self.session.found_rows = capabilities.contains(Capabilities::FOUND_ROWS);
    }

    fn connected_charset(&mut self, charset: u8) {
        self.session.negotiated_charset(charset);
    }

    fn query(&mut self, sql: &str) -> HandlerResult {
        self.run(sql, &[])
    }

    fn query_multi(&mut self, sql: &str, allow_multi: bool) -> Answered {
        if !allow_multi {
            // The client cannot read past the first result, so running the
            // rest would apply changes it never learns about.
            return match self.run(sql, &[]) {
                Ok(response) => Answered {
                    responses: vec![response],
                    error: None,
                },
                Err(e) => Answered {
                    responses: Vec::new(),
                    error: Some(e),
                },
            };
        }
        let executed = self.engine.execute_many(&mut self.session, sql, &[]);
        // Each statement's own count, which is why `execute_many` collects
        // them as it goes: by the time the batch is over the diagnostics area
        // holds only the last statement's conditions.
        let responses = executed
            .outcomes
            .into_iter()
            .zip(executed.warnings)
            .map(|(outcome, warnings)| Self::shape(outcome, warnings))
            .collect();
        Answered {
            responses,
            error: executed.error.map(ErrPacket::from),
        }
    }

    fn prepare(&mut self, id: u32, sql: &str) -> Result<Prepared, ErrPacket> {
        let param_count = count_placeholders(sql);
        Ok(Prepared {
            id,
            sql: sql.to_owned(),
            param_count,
            columns: self.declared_columns(sql, param_count)?,
        })
    }

    fn execute(&mut self, stmt: &Prepared, params: &[WireValue]) -> HandlerResult {
        // The client cached the columns this statement declared at prepare
        // time, and DDL since then can have made them wrong — a dropped column
        // it still expects, a renamed one it will look up by the old name.
        // MySQL answers 1615 and the client re-prepares.
        //
        // A re-describe rather than a schema-version counter. Describing is
        // ~8us against ~275us for the execution it guards, so the check costs
        // about 3% of a prepared read and nothing at all for a write; a
        // counter would be cheaper still, but it is a second source of truth
        // about when the schema changed, and this engine can afford to have
        // one. #0032 records the trade if that stops being true.
        //
        // A statement that declared nothing is skipped, which is every write.
        // There is no cached metadata to have gone stale, and binding a wide
        // INSERT's placeholders only to parse the result and throw it away is
        // most of the cost of the insert itself.
        //
        // A re-describe that now *refuses* is the same answer: the statement
        // no longer prepares, so the cached metadata cannot be right. It is
        // reported as 1615 rather than passed through, so the client takes the
        // path it already has — re-prepare, and hear the real reason there.
        if !stmt.columns.is_empty() {
            let fresh = self.declared_columns(&stmt.sql, stmt.param_count);
            if fresh.as_ref() != Ok(&stmt.columns) {
                return Err(ErrPacket::new(
                    1615,
                    "HY000",
                    "Prepared statement needs to be re-prepared",
                ));
            }
        }

        let bound: Vec<EngineValue> = params.iter().map(EngineValue::from_wire).collect();
        let sql = stmt.sql.clone();
        self.run(&sql, &bound)
    }

    fn init_db(&mut self, db: &str) -> Result<(), ErrPacket> {
        match self
            .engine
            .execute(&mut self.session, &format!("USE `{db}`"), &[])
        {
            Ok(_) => Ok(()),
            Err(e) => Err(ErrPacket::from(e)),
        }
    }

    fn reset(&mut self) {
        // MySQL keeps the schema the connection was opened with. Poolers reset
        // between checkouts, so dropping it here would make every subsequent
        // unqualified query fail with "no database selected".
        let config = self.session.config.clone();
        let id = self.session.connection_id;
        let database = self.session.current_database.clone();
        // Carried over for a different reason than the schema: capabilities are
        // settled by the handshake and COM_RESET_CONNECTION does not
        // renegotiate, so a connection that asked for CLIENT_FOUND_ROWS still
        // has it afterwards. Dropping it would silently change what every
        // later UPDATE on that connection reports.
        let found_rows = self.session.found_rows;
        self.session = Session::new(config, id);
        self.session.current_database = database;
        self.session.found_rows = found_rows;
    }

    fn statistics(&self) -> String {
        format!("mysql-ultralight {}", env!("CARGO_PKG_VERSION"))
    }
}

/// Counts `?` placeholders outside strings, identifiers and comments.
///
/// The count must match what the client will send, or the connection
/// desynchronises on the next COM_STMT_EXECUTE. It must also match what the
/// engine *binds*, or every parameter after the first disagreement lands in
/// the wrong place — which is why this asks the engine's own scanner rather
/// than keeping a second copy of it here. The two were separate copies, alike
/// enough to look maintained and free to drift on the next comment syntax
/// either one learned about.
///
/// Saturating rather than wrapping: `COM_STMT_PREPARE` has a 16-bit parameter
/// count, and a statement with more markers than that cannot be told to the
/// client honestly in either direction. Reporting the cap is the answer that
/// leaves the client complaining about the count rather than reading a short
/// one and desynchronising.
fn count_placeholders(sql: &str) -> u16 {
    mysql_ultralight_engine::params::placeholders(sql)
        .len()
        .try_into()
        .unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> EngineHandler {
        let engine = Engine::new(ServerConfig::default());
        engine.bootstrap_database("t").unwrap();
        let mut h = EngineHandler::new(engine, ServerConfig::default(), 1);
        h.run("USE `t`", &[]).unwrap();
        h
    }

    fn names(stmt: &Prepared) -> Vec<String> {
        stmt.columns.iter().map(|c| c.name.clone()).collect()
    }

    #[test]
    fn a_prepare_declares_the_columns_the_client_will_cache() {
        let mut h = handler();
        h.run("CREATE TABLE T (id INT, a VARCHAR(10))", &[])
            .unwrap();

        let stmt = h.prepare(1, "SELECT * FROM T WHERE id = ?").unwrap();
        assert_eq!(stmt.param_count, 1);
        assert_eq!(names(&stmt), vec!["id", "a"]);

        // A statement that cannot be shaped without running something that
        // might write declares nothing rather than guessing.
        let write = h.prepare(2, "INSERT INTO T VALUES (?, ?)").unwrap();
        assert!(write.columns.is_empty());
        assert_eq!(write.param_count, 2);
    }

    #[test]
    fn ddl_under_a_prepared_statement_asks_the_client_to_re_prepare() {
        let mut h = handler();
        h.run("CREATE TABLE T (id INT, a VARCHAR(10))", &[])
            .unwrap();
        h.run("INSERT INTO T VALUES (1, 'x')", &[]).unwrap();

        let stmt = h.prepare(1, "SELECT * FROM T WHERE id = ?").unwrap();
        assert_eq!(names(&stmt), vec!["id", "a"]);
        assert!(h.execute(&stmt, &[WireValue::I64(1)]).is_ok());

        // The client is holding metadata that says two columns. Answering with
        // three would hand it a row it cannot read by name.
        h.run("ALTER TABLE T ADD COLUMN b INT", &[]).unwrap();
        let err = h.execute(&stmt, &[WireValue::I64(1)]).unwrap_err();
        assert_eq!(err.code, 1615);

        // Re-preparing is what the client does next, and it works.
        let stmt = h.prepare(1, "SELECT * FROM T WHERE id = ?").unwrap();
        assert_eq!(names(&stmt), vec!["id", "a", "b"]);
        assert!(h.execute(&stmt, &[WireValue::I64(1)]).is_ok());

        // A dropped table is a change too, rather than a stale answer.
        h.run("DROP TABLE T", &[]).unwrap();
        assert_eq!(
            h.execute(&stmt, &[WireValue::I64(1)]).unwrap_err().code,
            1615
        );
    }

    #[test]
    fn ordinary_writes_do_not_invalidate_a_prepared_statement() {
        let mut h = handler();
        h.run("CREATE TABLE T (id INT, a VARCHAR(10))", &[])
            .unwrap();

        let read = h.prepare(1, "SELECT id, a FROM T WHERE id = ?").unwrap();
        let write = h.prepare(2, "INSERT INTO T VALUES (?, ?)").unwrap();

        // Rows changing is not a schema change, and re-preparing on every
        // insert would make the check worse than the problem.
        for i in 0..5 {
            h.execute(&write, &[WireValue::I64(i), WireValue::Str("x".to_owned())])
                .unwrap();
            assert!(h.execute(&read, &[WireValue::I64(i)]).is_ok());
        }
    }

    /// The affected count an OK packet carries, so what is asserted is what
    /// the client would read rather than a field of the session.
    fn affected(h: &mut EngineHandler, sql: &str) -> u64 {
        match h.run(sql, &[]).unwrap() {
            Response::Ok(ok) => ok.affected_rows,
            Response::ResultSet(_) => panic!("expected an affected count from: {sql}"),
        }
    }

    /// #0089. `CLIENT_FOUND_ROWS` decides whether an UPDATE reports the rows it
    /// matched or the rows it changed, and the handshake is the only place that
    /// number is known — so a handler that never learns the capability answers
    /// every client the same way. Both branches are reached in practice:
    /// node `mysql2` and Prisma's query engine both set the flag, while the
    /// `mysql` command-line client does not — measured off this server's
    /// handshake trace, one connection each.
    #[test]
    fn the_negotiated_capabilities_decide_what_an_update_reports() {
        for found_rows in [false, true] {
            let mut h = handler();
            h.negotiated(if found_rows {
                Capabilities::FOUND_ROWS
            } else {
                Capabilities::EMPTY
            });
            h.run("CREATE TABLE T (id INT, a VARCHAR(10))", &[])
                .unwrap();
            h.run("INSERT INTO T VALUES (1, 'x'), (2, 'x')", &[])
                .unwrap();

            let matched_but_unchanged = affected(&mut h, "UPDATE T SET a = 'x'");
            assert_eq!(
                matched_but_unchanged,
                if found_rows { 2 } else { 0 },
                "with FOUND_ROWS = {found_rows}"
            );

            // COM_RESET_CONNECTION does not renegotiate, so the capability
            // outlives it. Dropping it here would silently change what every
            // later UPDATE on a pooled connection reports.
            h.reset();
            assert_eq!(
                affected(&mut h, "UPDATE T SET a = 'x'"),
                matched_but_unchanged,
                "after reset, with FOUND_ROWS = {found_rows}"
            );
        }
    }

    /// #0204. The count the client actually reads, taken off the packet
    /// rather than off the session -- the field was hard-coded 0, so a
    /// truncating write reached every client as a clean success.
    ///
    /// Both shapes, because they carry it in different packets: a write in the
    /// OK packet's own field, a statement returning rows in the packet that
    /// terminates the result set.
    #[test]
    fn the_wire_carries_the_count_of_what_a_statement_raised() {
        let mut h = handler();
        h.run("SET SESSION sql_mode = ''", &[]).unwrap();
        h.run("CREATE TABLE T (a VARCHAR(3))", &[]).unwrap();

        let warnings = |response: &Response| match response {
            Response::Ok(ok) => ok.warnings,
            Response::ResultSet(rs) => rs.warnings,
        };

        // Two of the three rows are too long for the column.
        let write = h
            .run("INSERT INTO T VALUES ('abcdef'), ('ok'), ('zzzzzz')", &[])
            .unwrap();
        assert_eq!(warnings(&write), 2);

        // A reader leaves the area alone, so the count it reports is the same
        // one -- which is also what 8.3 does.
        let read = h.run("SHOW WARNINGS", &[]).unwrap();
        assert_eq!(warnings(&read), 2);

        // And a statement that raises nothing reports nothing, because it
        // replaced the area rather than appending to it.
        let clean = h.run("SELECT 1", &[]).unwrap();
        assert_eq!(warnings(&clean), 0);
    }

    /// A batch reports each statement's own count, not the last one's. The
    /// diagnostics area holds only the final statement's conditions by the
    /// time the batch is over, so a count read at the end would tell the
    /// client every statement in a migration raised whatever the last one did.
    #[test]
    fn every_statement_of_a_batch_reports_its_own_count() {
        let mut h = handler();
        let answered = h.query_multi(
            "SET SESSION sql_mode = ''; \
             CREATE TABLE T (a VARCHAR(3)); \
             INSERT INTO T VALUES ('abcdef'), ('zzzzzz'); \
             SELECT 1",
            true,
        );
        assert!(answered.error.is_none(), "{:?}", answered.error);
        let counts: Vec<u16> = answered
            .responses
            .iter()
            .map(|response| match response {
                Response::Ok(ok) => ok.warnings,
                Response::ResultSet(rs) => rs.warnings,
            })
            .collect();
        assert_eq!(counts, vec![0, 0, 2, 0]);
    }

    /// 8.3 refuses the diagnostics statements in the prepared-statement
    /// protocol with 1295, measured -- so a client that prepares `SHOW
    /// WARNINGS` gets an error there and must get one here. Answering it would
    /// be this engine passing a statement a real server refuses, which is the
    /// direction of gap it exists to close.
    #[test]
    fn the_diagnostics_statements_do_not_prepare() {
        let mut h = handler();
        for sql in [
            "SHOW WARNINGS",
            "SHOW WARNINGS LIMIT 1",
            "SHOW ERRORS",
            "SHOW COUNT(*) WARNINGS",
            "SHOW COUNT(*) ERRORS",
        ] {
            let refused = h
                .prepare(1, sql)
                .expect_err("the prepared protocol refuses this");
            assert_eq!(refused.code, 1295, "{sql}");
        }
        // And the text protocol still answers them, which is the half that
        // would be lost by refusing in the wrong place.
        assert!(matches!(
            h.query("SHOW WARNINGS"),
            Ok(Response::ResultSet(_))
        ));
    }

    #[test]
    fn counts_only_real_placeholders() {
        assert_eq!(count_placeholders("SELECT ?, ?, ?"), 3);
        assert_eq!(count_placeholders("SELECT '?' , ? -- ?\n, `we?rd`, ?"), 2);
        assert_eq!(count_placeholders("SELECT /* ? */ ?"), 1);
        assert_eq!(count_placeholders("SELECT 'it''s ?', ?"), 1);
    }
}
