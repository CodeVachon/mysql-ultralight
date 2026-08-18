//! Adapts the engine to the server's `Handler` trait.
//!
//! The engine knows nothing about packets and the server knows nothing about
//! SQL; this is the only place the two meet.

use std::sync::Arc;

use mysql_ultralight_engine::value::Value as EngineValue;
use mysql_ultralight_engine::{Engine, Outcome, ServerConfig, Session};
use mysql_ultralight_protocol::{ErrPacket, OkPacket, Value as WireValue};
use mysql_ultralight_server::{Handler, HandlerResult, Prepared, Response, ResultSet};

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
        match self.engine.execute(&mut self.session, sql, parameters) {
            Ok(Outcome::Rows(rows)) => {
                let columns = rows.columns.iter().map(|c| c.to_wire()).collect();
                let wire_rows = rows
                    .rows
                    .into_iter()
                    .map(|row| row.iter().map(EngineValue::to_wire).collect())
                    .collect();
                Ok(Response::ResultSet(ResultSet::with_rows(
                    columns, wire_rows,
                )))
            }
            Ok(Outcome::Affected {
                rows,
                last_insert_id,
            }) => Ok(Response::Ok(OkPacket::new(rows, last_insert_id))),
            Err(e) => Err(ErrPacket::from(e)),
        }
    }
}

impl Handler for EngineHandler {
    fn query(&mut self, sql: &str) -> HandlerResult {
        self.run(sql, &[])
    }

    fn prepare(&mut self, id: u32, sql: &str) -> Result<Prepared, ErrPacket> {
        Ok(Prepared {
            id,
            sql: sql.to_owned(),
            param_count: count_placeholders(sql),
            // Column metadata is not known until the statement runs, which is
            // also what MySQL reports for a statement it cannot resolve early.
            columns: Vec::new(),
        })
    }

    fn execute(&mut self, stmt: &Prepared, params: &[WireValue]) -> HandlerResult {
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
    use super::count_placeholders;

    #[test]
    fn counts_only_real_placeholders() {
        assert_eq!(count_placeholders("SELECT ?, ?, ?"), 3);
        assert_eq!(count_placeholders("SELECT '?' , ? -- ?\n, `we?rd`, ?"), 2);
        assert_eq!(count_placeholders("SELECT /* ? */ ?"), 1);
        assert_eq!(count_placeholders("SELECT 'it''s ?', ?"), 1);
    }
}
