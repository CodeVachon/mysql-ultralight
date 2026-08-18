//! A stand-in query handler for the wire protocol viability spike.
//!
//! This is not the engine. It answers the handful of introspection queries a
//! driver runs while connecting, plus `SELECT 1`, and returns an empty result
//! set for anything else so a client gets as far as it can. Every statement is
//! traced, which is the point: the spike exists to discover exactly what real
//! clients demand before any of it is built for real.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use mysql_ultralight_protocol::{ColumnDef, ColumnType, ErrPacket, Value};
use mysql_ultralight_server::{Handler, HandlerResult, Prepared, Response, ResultSet};

/// Statements the handler did not recognise, shared across connections so the
/// CLI can print a summary on shutdown.
#[derive(Debug, Default)]
pub struct Unhandled {
    count: AtomicUsize,
    statements: std::sync::Mutex<Vec<String>>,
}

impl Unhandled {
    pub fn record(&self, sql: &str) {
        self.count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut list) = self.statements.lock() {
            if !list.iter().any(|s| s == sql) {
                list.push(sql.to_owned());
            }
        }
    }

    pub fn report(&self) -> Vec<String> {
        self.statements
            .lock()
            .map(|l| l.clone())
            .unwrap_or_default()
    }

    pub fn total(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }
}

pub struct SpikeHandler {
    version: String,
    socket: Option<String>,
    port: u16,
    database: Option<String>,
    unhandled: Arc<Unhandled>,
}

impl SpikeHandler {
    pub fn new(version: impl Into<String>, unhandled: Arc<Unhandled>) -> Self {
        Self {
            version: version.into(),
            socket: None,
            port: 0,
            database: None,
            unhandled,
        }
    }

    pub fn with_endpoint(mut self, socket: Option<String>, port: u16) -> Self {
        self.socket = socket;
        self.port = port;
        self
    }

    fn one_column(name: &str, ty: ColumnType, value: Value) -> Response {
        Response::ResultSet(ResultSet::with_rows(
            vec![ColumnDef::new(name, ty)],
            vec![vec![value]],
        ))
    }

    /// Resolves a system variable to its column type and value.
    ///
    /// Drivers read these during connection setup and branch on the answers,
    /// so the values must be plausible and the types must be right: returning
    /// a number as a string is enough to break a client.
    fn system_variable(&self, name: &str) -> Option<(ColumnType, Value)> {
        let int = |v: i64| Some((ColumnType::LongLong, Value::I64(v)));
        let text = |v: &str| Some((ColumnType::VarString, Value::Str(v.to_owned())));

        match name {
            "version" => text(&self.version),
            "version_comment" => text("mysql-ultralight (test engine)"),
            "socket" => text(self.socket.as_deref().unwrap_or("")),
            "max_allowed_packet" => int(67_108_864),
            "wait_timeout" | "interactive_timeout" => int(28_800),
            "net_write_timeout" | "net_read_timeout" => int(60),
            "sql_mode" => text(
                "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,\
ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION",
            ),
            "autocommit" => int(1),
            "foreign_key_checks" => int(1),
            "lower_case_table_names" => int(0),
            "max_connections" => int(151),
            "transaction_isolation" | "tx_isolation" => text("REPEATABLE-READ"),
            "time_zone" => text("SYSTEM"),
            "system_time_zone" => text("UTC"),
            "character_set_client"
            | "character_set_connection"
            | "character_set_results"
            | "character_set_server"
            | "character_set_database" => text("utf8mb4"),
            "collation_connection" | "collation_server" | "collation_database" => {
                text("utf8mb4_0900_ai_ci")
            }
            "hostname" => text("localhost"),
            "port" => int(self.port as i64),
            "license" => text("MIT OR Apache-2.0"),
            "have_ssl" | "have_openssl" => text("DISABLED"),
            "performance_schema" => int(0),
            "sql_require_primary_key" => int(0),
            _ => None,
        }
    }

    /// Handles `SELECT @@a, @@b, ...`, the shape drivers use to probe the
    /// server during connection setup.
    ///
    /// Returns `None` unless every item in the select list is a system
    /// variable, so ordinary queries fall through untouched.
    fn select_system_variables(&self, sql: &str) -> Option<Response> {
        let rest = sql.get(..6).filter(|p| p.eq_ignore_ascii_case("select"))?;
        let _ = rest;
        let list = sql[6..].trim();
        if list.is_empty() || !list.starts_with("@@") {
            return None;
        }

        let mut columns = Vec::new();
        let mut row = Vec::new();

        for item in list.split(',') {
            let item = item.trim();
            // An alias may be introduced with AS or, as Prisma's migration
            // engine does in `SELECT @@GLOBAL.version version`, by juxtaposition.
            let mut parts = item.split_whitespace();
            let expr = parts.next()?;
            let alias = match parts.next() {
                Some(word) if word.eq_ignore_ascii_case("as") => parts.next(),
                other => other,
            };

            let raw = expr.strip_prefix("@@")?;
            // `@@GLOBAL.x` and `@@SESSION.x` resolve the same here: this engine
            // has a single session and no server-wide configuration to differ.
            let bare = raw
                .split_once('.')
                .filter(|(scope, _)| {
                    matches!(
                        scope.to_ascii_lowercase().as_str(),
                        "global" | "session" | "local"
                    )
                })
                .map_or(raw, |(_, name)| name);

            let (ty, value) = self.system_variable(&bare.to_ascii_lowercase())?;
            let name = alias
                .map(|a| {
                    a.trim_matches(|c| c == '`' || c == '\'' || c == '"')
                        .to_owned()
                })
                .unwrap_or_else(|| expr.to_owned());

            columns.push(ColumnDef::new(name, ty).nullable());
            row.push(value);
        }

        Some(Response::ResultSet(ResultSet::with_rows(
            columns,
            vec![row],
        )))
    }

    /// Answers the statement, or returns `None` if it is not recognised.
    fn dispatch(&mut self, sql: &str) -> Option<Response> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let lower = trimmed.to_ascii_lowercase();

        // Statements that only need acknowledgement.
        if lower.starts_with("set ")
            || lower.starts_with("use ")
            || matches!(
                lower.as_str(),
                "begin" | "commit" | "rollback" | "start transaction" | "ping"
            )
        {
            return Some(Response::ok());
        }

        if lower == "select 1" || lower == "select 1 as `1`" || lower == "do 1" {
            return Some(Self::one_column("1", ColumnType::LongLong, Value::I64(1)));
        }

        if let Some(response) = self.select_system_variables(trimmed) {
            return Some(response);
        }

        if lower == "select version()" {
            return Some(Self::one_column(
                "version()",
                ColumnType::VarString,
                Value::Str(self.version.clone()),
            ));
        }

        if lower == "select database()" || lower == "select schema()" {
            let value = self.database.clone().map_or(Value::Null, Value::Str);
            return Some(Self::one_column("database()", ColumnType::VarString, value));
        }

        if lower == "select connection_id()" {
            return Some(Self::one_column(
                "connection_id()",
                ColumnType::LongLong,
                Value::I64(1),
            ));
        }

        if lower.starts_with("show warnings") || lower.starts_with("show errors") {
            return Some(Response::ResultSet(ResultSet::new(vec![
                ColumnDef::new("Level", ColumnType::VarString),
                ColumnDef::new("Code", ColumnType::Long),
                ColumnDef::new("Message", ColumnType::VarString),
            ])));
        }

        if lower.starts_with("show variables") || lower.starts_with("show session variables") {
            return Some(Response::ResultSet(ResultSet::with_rows(
                vec![
                    ColumnDef::new("Variable_name", ColumnType::VarString),
                    ColumnDef::new("Value", ColumnType::VarString),
                ],
                vec![
                    vec![
                        Value::Str("version".to_owned()),
                        Value::Str(self.version.clone()),
                    ],
                    vec![
                        Value::Str("character_set_client".to_owned()),
                        Value::Str("utf8mb4".to_owned()),
                    ],
                    vec![
                        Value::Str("lower_case_table_names".to_owned()),
                        Value::Str("0".to_owned()),
                    ],
                ],
            )));
        }

        None
    }
}

impl Handler for SpikeHandler {
    fn query(&mut self, sql: &str) -> HandlerResult {
        if let Some(response) = self.dispatch(sql) {
            return Ok(response);
        }
        self.unhandled.record(sql.trim());
        // An empty result set rather than an error: the goal is to let the
        // client proceed and reveal everything it asks for in one run.
        Ok(Response::ResultSet(ResultSet::new(vec![ColumnDef::new(
            "unsupported",
            ColumnType::VarString,
        )])))
    }

    fn prepare(&mut self, id: u32, sql: &str) -> Result<Prepared, ErrPacket> {
        Ok(Prepared {
            id,
            sql: sql.to_owned(),
            param_count: count_placeholders(sql),
            // Leaving columns empty defers metadata to execution time, which
            // MySQL also does for statements it cannot resolve early.
            columns: Vec::new(),
        })
    }

    fn execute(&mut self, stmt: &Prepared, params: &[Value]) -> HandlerResult {
        // `SELECT ?` echoes its parameter back, which is the cheapest possible
        // end-to-end proof that binary parameter decoding is correct.
        if stmt
            .sql
            .trim()
            .trim_end_matches(';')
            .eq_ignore_ascii_case("select ?")
        {
            let value = params.first().cloned().unwrap_or(Value::Null);
            return Ok(Response::ResultSet(ResultSet::with_rows(
                vec![ColumnDef::new("?", value.inferred_type()).nullable()],
                vec![vec![value]],
            )));
        }
        self.query(&stmt.sql)
    }

    fn init_db(&mut self, db: &str) -> Result<(), ErrPacket> {
        self.database = Some(db.to_owned());
        Ok(())
    }

    fn reset(&mut self) {
        self.database = None;
    }
}

/// Counts `?` placeholders, skipping those inside string literals, quoted
/// identifiers, and comments.
///
/// A real implementation gets this from the parser. This is only good enough
/// to keep the spike's prepared statements in sync with the client.
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
                        // A doubled quote is an escaped quote, not a close.
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
    fn counts_bare_placeholders() {
        assert_eq!(count_placeholders("SELECT ?, ?, ?"), 3);
    }

    #[test]
    fn ignores_placeholders_inside_literals_and_comments() {
        assert_eq!(count_placeholders("SELECT '?' , ? -- ?\n, `we?rd`, ?"), 2);
        assert_eq!(count_placeholders("SELECT /* ? */ ?"), 1);
        assert_eq!(count_placeholders("SELECT 'it''s ?', ?"), 1);
    }
}
