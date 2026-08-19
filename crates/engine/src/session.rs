//! Per-connection state: the current database, system variables, and the
//! user-defined variables that dynamic SQL depends on.

use std::collections::BTreeMap;

use crate::value::Value;

/// Server configuration a session reports and can override.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub version: String,
    pub port: u16,
    pub socket: Option<String>,
    /// The mode this server reports and parses under. The target deployment
    /// does not use MySQL's default, and two of its flags change what a query
    /// means rather than how it behaves.
    pub sql_mode: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            version: "8.3.0-mysql-ultralight".to_owned(),
            port: 0,
            socket: None,
            sql_mode: "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,\
                       ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION"
                .to_owned(),
        }
    }
}

/// State belonging to one connection.
#[derive(Debug, Clone)]
pub struct Session {
    pub config: ServerConfig,
    pub current_database: Option<String>,
    pub connection_id: u32,
    /// `@name` variables. These outlive the statement that set them, which is
    /// what makes the PREPARE/EXECUTE idiom in the migrations work.
    pub user_variables: BTreeMap<String, Value>,
    /// Session-scoped overrides of system variables.
    overrides: BTreeMap<String, String>,
    /// Statements held by `PREPARE`, keyed by name. Server-side prepared
    /// statements are how the migrations express conditional DDL, and they
    /// outlive the statement that created them.
    pub prepared: BTreeMap<String, String>,
    pub in_transaction: bool,
    /// The catalog as it stood when the transaction opened, plus any named
    /// savepoints taken since.
    ///
    /// A whole-catalog copy is the honest trade for an in-memory test engine:
    /// the data is small, and undo logging would be a great deal of machinery
    /// for a database that discards everything at process exit.
    pub transaction: Option<Transaction>,
    /// How many views deep evaluation currently is, so a self-referential view
    /// is refused rather than exhausting the stack.
    pub view_depth: usize,
    /// The row a lateral factor is being evaluated against.
    ///
    /// `JSON_TABLE(t.doc, ...)` is expanded once per row of the tables named
    /// before it, and this is how that row reaches the expression — the same
    /// shape a correlated subquery uses, because it is the same problem.
    pub lateral: Option<crate::expr::OuterRow>,
    /// Common table expressions in scope, innermost last.
    ///
    /// A CTE is named like a table but exists only for the statement that
    /// declares it, so it lives here rather than in the catalog. Kept as
    /// already-materialised rows because MySQL evaluates a non-recursive CTE
    /// once however many times the body names it.
    pub ctes: Vec<(String, crate::relation::Relation)>,
    /// The parsed form of `config.sql_mode`, with the string it came from so a
    /// direct mutation of `config` cannot leave it stale.
    parsed_mode: crate::dialect::SqlMode,
    parsed_from: String,
    pub last_insert_id: u64,
    pub affected_rows: u64,
}

impl Session {
    pub fn new(config: ServerConfig, connection_id: u32) -> Self {
        let parsed_mode = crate::dialect::SqlMode::parse(&config.sql_mode);
        let parsed_from = config.sql_mode.clone();
        Self {
            config,
            current_database: None,
            connection_id,
            user_variables: BTreeMap::new(),
            overrides: BTreeMap::new(),
            prepared: BTreeMap::new(),
            in_transaction: false,
            transaction: None,
            view_depth: 0,
            lateral: None,
            ctes: Vec::new(),
            parsed_mode,
            parsed_from,
            last_insert_id: 0,
            affected_rows: 0,
        }
    }

    /// The parsing mode this session runs under.
    pub fn sql_mode(&self) -> crate::dialect::SqlMode {
        // Parsing the mode allocates per flag and `||` is evaluated once per
        // row, so the result is cached. `config` is public, so the cache is
        // validated against the string rather than trusting every caller to
        // have gone through `set_system_variable`: a stale mode would silently
        // change what a query means.
        if self.parsed_from == self.config.sql_mode {
            self.parsed_mode
        } else {
            crate::dialect::SqlMode::parse(&self.config.sql_mode)
        }
    }

    /// Recomputes the cached mode after the configuration changes.
    fn refresh_mode(&mut self) {
        self.parsed_from = self.config.sql_mode.clone();
        self.parsed_mode = crate::dialect::SqlMode::parse(&self.config.sql_mode);
    }

    pub fn set_user_variable(&mut self, name: &str, value: Value) {
        self.user_variables.insert(name.to_ascii_lowercase(), value);
    }

    /// Unset user variables read as NULL rather than erroring, as in MySQL.
    pub fn user_variable(&self, name: &str) -> Value {
        self.user_variables
            .get(&name.to_ascii_lowercase())
            .cloned()
            .unwrap_or(Value::Null)
    }

    pub fn set_system_variable(&mut self, name: &str, value: String) {
        let name = normalize_scope(name);
        if name == "sql_mode" {
            self.config.sql_mode = value.clone();
            self.refresh_mode();
        }
        self.overrides.insert(name, value);
    }

    /// Resolves a system variable.
    ///
    /// Every driver reads several of these while connecting, and Prisma's
    /// panics rather than errors when the answer is an empty result set, so
    /// unknown names still return a value where MySQL defines one.
    pub fn system_variable(&self, name: &str) -> Option<Value> {
        let name = normalize_scope(name);
        if let Some(value) = self.overrides.get(&name) {
            return Some(Value::Text(value.clone()));
        }

        let text = |s: &str| Some(Value::Text(s.to_owned()));
        let int = |v: i64| Some(Value::Int(v));

        match name.as_str() {
            "version" => text(&self.config.version),
            "version_comment" => text("mysql-ultralight (test engine)"),
            "version_compile_os" => text(std::env::consts::OS),
            "version_compile_machine" => text(std::env::consts::ARCH),
            "socket" => text(self.config.socket.as_deref().unwrap_or("")),
            "port" => int(self.config.port as i64),
            "hostname" => text("localhost"),
            "sql_mode" => text(&self.config.sql_mode),
            // 64MB, matching the server's own message cap.
            "max_allowed_packet" => int(67_108_864),
            "wait_timeout" | "interactive_timeout" => int(28_800),
            "net_write_timeout" | "net_read_timeout" => int(60),
            "max_connections" => int(151),
            "autocommit" => int(1),
            "foreign_key_checks" => int(1),
            "unique_checks" => int(1),
            "lower_case_table_names" => int(0),
            "sql_require_primary_key" => int(0),
            "transaction_isolation" | "tx_isolation" => text("REPEATABLE-READ"),
            "transaction_read_only" | "tx_read_only" => int(0),
            "time_zone" => text("SYSTEM"),
            "system_time_zone" => text("UTC"),
            "character_set_client"
            | "character_set_connection"
            | "character_set_results"
            | "character_set_server"
            | "character_set_database"
            | "character_set_system" => text("utf8mb4"),
            "collation_connection" | "collation_server" | "collation_database" => {
                text("utf8mb4_0900_ai_ci")
            }
            "license" => text("MIT OR Apache-2.0"),
            "have_ssl" | "have_openssl" => text("DISABLED"),
            "performance_schema" => int(0),
            "innodb_version" => text(&self.config.version),
            "protocol_version" => int(10),
            "auto_increment_increment" | "auto_increment_offset" => int(1),
            "group_concat_max_len" => int(1024),
            "max_execution_time" => int(0),
            "default_storage_engine" => text("InnoDB"),
            "init_connect" => text(""),
            "last_insert_id" => int(self.last_insert_id as i64),
            "identity" => int(self.last_insert_id as i64),
            _ => None,
        }
    }

    /// Every system variable the engine knows, for `SHOW VARIABLES`.
    pub fn known_system_variables(&self) -> Vec<(String, String)> {
        const NAMES: &[&str] = &[
            "auto_increment_increment",
            "autocommit",
            "character_set_client",
            "character_set_connection",
            "character_set_database",
            "character_set_results",
            "character_set_server",
            "collation_connection",
            "collation_database",
            "collation_server",
            "default_storage_engine",
            "foreign_key_checks",
            "group_concat_max_len",
            "have_ssl",
            "hostname",
            "innodb_version",
            "interactive_timeout",
            "license",
            "lower_case_table_names",
            "max_allowed_packet",
            "max_connections",
            "max_execution_time",
            "net_read_timeout",
            "net_write_timeout",
            "performance_schema",
            "port",
            "protocol_version",
            "socket",
            "sql_mode",
            "sql_require_primary_key",
            "system_time_zone",
            "time_zone",
            "transaction_isolation",
            "unique_checks",
            "version",
            "version_comment",
            "version_compile_machine",
            "version_compile_os",
            "wait_timeout",
        ];

        let mut out: Vec<(String, String)> = NAMES
            .iter()
            .filter_map(|n| {
                self.system_variable(n)
                    .map(|v| ((*n).to_owned(), v.as_text()))
            })
            .collect();
        for (name, value) in &self.overrides {
            if !out.iter().any(|(n, _)| n == name) {
                out.push((name.clone(), value.clone()));
            }
        }
        out.sort();
        out
    }
}

/// A transaction's undo state.
/// A named point a transaction can be rolled back to.
#[derive(Debug, Clone)]
pub struct Savepoint {
    pub name: String,
    /// The catalog as it stood when the savepoint was taken.
    pub catalog: crate::catalog::Catalog,
    /// Tables written before it, so a rollback knows what to restore.
    pub modified: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Transaction {
    /// The catalog at BEGIN, restored by ROLLBACK.
    pub opened: crate::catalog::Catalog,
    /// Named savepoints, in the order they were taken.
    pub savepoints: Vec<Savepoint>,
    /// The `(database, table)` pairs this transaction has written to.
    ///
    /// Rollback restores only these. Replacing the whole catalog would discard
    /// work another connection had already committed, since the catalog is
    /// shared — turning a rollback into data loss for someone else.
    pub modified: Vec<(String, String)>,
}

/// Strips a `GLOBAL.` / `SESSION.` / `LOCAL.` prefix and lowercases the rest.
///
/// This engine has one session and no server-wide configuration to differ from
/// it, so the scopes resolve identically.
fn normalize_scope(name: &str) -> String {
    let lower = name.trim_start_matches("@@").to_ascii_lowercase();
    match lower.split_once('.') {
        Some(("global" | "session" | "local", rest)) => rest.to_owned(),
        _ => lower,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_resolve_to_the_same_variable() {
        let s = Session::new(ServerConfig::default(), 1);
        let direct = s.system_variable("version");
        assert_eq!(s.system_variable("@@GLOBAL.version"), direct);
        assert_eq!(s.system_variable("SESSION.version"), direct);
        assert_eq!(s.system_variable("@@version"), direct);
    }

    /// Prisma opens every connection with these three. An empty result makes
    /// its driver panic rather than return an error.
    #[test]
    fn the_prisma_connection_preamble_resolves() {
        let s = Session::new(ServerConfig::default(), 1);
        for name in ["socket", "max_allowed_packet", "wait_timeout"] {
            assert!(s.system_variable(name).is_some(), "{name} must resolve");
        }
    }

    #[test]
    fn an_unknown_system_variable_is_none_rather_than_a_guess() {
        let s = Session::new(ServerConfig::default(), 1);
        assert_eq!(s.system_variable("not_a_real_variable"), None);
    }

    #[test]
    fn user_variables_outlive_the_statement_that_set_them() {
        let mut s = Session::new(ServerConfig::default(), 1);
        assert_eq!(s.user_variable("x"), Value::Null);
        s.set_user_variable("x", Value::Int(7));
        assert_eq!(s.user_variable("X"), Value::Int(7));
    }

    #[test]
    fn setting_sql_mode_updates_what_the_parser_will_see() {
        let mut s = Session::new(ServerConfig::default(), 1);
        s.set_system_variable("sql_mode", "ANSI_QUOTES".to_owned());
        assert_eq!(s.config.sql_mode, "ANSI_QUOTES");
        assert_eq!(
            s.system_variable("sql_mode"),
            Some(Value::Text("ANSI_QUOTES".to_owned()))
        );
    }
}
