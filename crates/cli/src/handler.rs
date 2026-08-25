//! Adapts the engine to the server's `Handler` trait.
//!
//! The engine knows nothing about packets and the server knows nothing about
//! SQL; this is the only place the two meet.

use std::sync::Arc;

use mysql_ultralight_engine::value::Value as EngineValue;
use mysql_ultralight_engine::{Engine, Outcome, ServerConfig, Session};
use mysql_ultralight_protocol::{ColumnDef, ErrPacket, OkPacket, Value as WireValue};
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
        Ok(Self::shape(outcome))
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
    fn shape(outcome: Outcome) -> Response {
        match outcome {
            Outcome::Rows(rows) => {
                let columns = rows.columns.iter().map(|c| c.to_wire()).collect();
                let wire_rows = rows
                    .rows
                    .into_iter()
                    .map(|row| row.iter().map(EngineValue::to_wire).collect())
                    .collect();
                Response::ResultSet(ResultSet::with_rows(columns, wire_rows))
            }
            Outcome::Affected {
                rows,
                last_insert_id,
            } => Response::Ok(OkPacket::new(rows, last_insert_id)),
        }
    }
}

impl Handler for EngineHandler {
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
        Answered {
            responses: executed.outcomes.into_iter().map(Self::shape).collect(),
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
        self.session = Session::new(config, id);
        self.session.current_database = database;
    }

    fn statistics(&self) -> String {
        format!("mysql-ultralight {}", env!("CARGO_PKG_VERSION"))
    }
}

/// Counts `?` placeholders outside strings, identifiers and comments.
///
/// The count must match what the client will send, or the connection
/// desynchronises on the next COM_STMT_EXECUTE.
fn count_placeholders(sql: &str) -> u16 {
    let bytes = sql.as_bytes();
    let mut count: u16 = 0;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && q != b'`' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == q {
                        if bytes.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'?' => {
                count = count.saturating_add(1);
                i += 1;
            }
            _ => i += 1,
        }
    }

    count
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

    #[test]
    fn counts_only_real_placeholders() {
        assert_eq!(count_placeholders("SELECT ?, ?, ?"), 3);
        assert_eq!(count_placeholders("SELECT '?' , ? -- ?\n, `we?rd`, ?"), 2);
        assert_eq!(count_placeholders("SELECT /* ? */ ?"), 1);
        assert_eq!(count_placeholders("SELECT 'it''s ?', ?"), 1);
    }
}
