//! Per-connection state: the current database, system variables, and the
//! user-defined variables that dynamic SQL depends on.

use std::collections::BTreeMap;

use crate::diagnostics::Diagnostics;
use crate::error::{EngineError, EngineResult};
use crate::value::Value;

/// The stock 8.3 value of `group_concat_max_len`, measured.
const DEFAULT_GROUP_CONCAT_MAX_LEN: u64 = 1024;

/// The smallest value 8.3 keeps for `group_concat_max_len`. Anything below is
/// clamped up to it, so a separator plus one short value always fits.
const MIN_GROUP_CONCAT_MAX_LEN: u64 = 4;

/// The stock 8.3 value of `cte_max_recursion_depth`, measured.
const DEFAULT_CTE_MAX_RECURSION_DEPTH: u64 = 1000;

/// The stock 8.3 value of `max_error_count`, measured.
const DEFAULT_MAX_ERROR_COUNT: u64 = 1024;

/// The largest value 8.3 keeps for `max_error_count`, measured: 99999999 is
/// stored as this and -1 as 0.
const MAX_MAX_ERROR_COUNT: i128 = 65535;

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
    /// Identifies this particular server process, reported as
    /// `@@ultralight_instance`.
    ///
    /// A harness that starts a server and then connects to a port cannot
    /// otherwise tell the process it started from a leftover one on the same
    /// port: both accept a connection and both call themselves
    /// mysql-ultralight. The differential harness compared MySQL against a
    /// previous build for exactly that reason and reported no divergences, so
    /// the answer to "are you the server I started" is a value only that
    /// process knows.
    pub instance: String,
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
            instance: new_instance_id(),
        }
    }
}

/// A value no other running server will report.
///
/// Mixed from three sources rather than one because each is defeatable alone:
/// the clock repeats across a restart of a suspended machine, the pid is
/// reused, and a fixed hash of the two is the same on every host. `RandomState`
/// is seeded per process by the OS, which is the part that makes two servers
/// started in the same millisecond differ.
fn new_instance_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hash, Hasher};

    let mut hasher = RandomState::new().build_hasher();
    std::process::id().hash(&mut hasher);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
    ///
    /// Boxed, which is a *stack* decision and not a heap one: see
    /// `a_session_stays_within_its_stack_budget`. This is the largest field
    /// here and the one almost every session leaves empty, so it is the one
    /// that pays for the diagnostics area below.
    pub transaction: Option<Box<Transaction>>,
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
    /// Whether this connection negotiated `CLIENT_FOUND_ROWS`.
    ///
    /// It decides what an UPDATE *means* by "affected": with the flag, MySQL
    /// reports the rows the WHERE matched; without it, the rows whose stored
    /// values actually changed. Measured on 8.3 over two rows already holding
    /// the value: `UPDATE U SET x = x` answers 2 with the flag and 0 without,
    /// and `ROW_COUNT()` follows the same rule. Both meanings are correct
    /// answers to different questions, so this is read rather than picked.
    /// Which branch a client lands on is not the obvious split: the `mysql`
    /// command-line client and the `mysql` crate the differential harness uses
    /// leave the flag off, while node `mysql2` sets it by default and Prisma's
    /// query engine sets it too — all four measured against this server's own
    /// handshake trace. So the *changed* meaning below is what an unadorned
    /// client gets and the matched meaning is what the drivers ask for, and
    /// both reach this engine.
    ///
    /// False by default, which is the meaning a client that says nothing gets.
    pub found_rows: bool,
    /// What the last statement that raised anything raised.
    ///
    /// Private because the rule about it is a rule about *when* it changes —
    /// a statement replaces it, a `SHOW WARNINGS` does not — and that rule
    /// lives in `Engine::execute` rather than in every writer. See
    /// `crate::diagnostics`.
    diagnostics: Diagnostics,
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
            found_rows: false,
            diagnostics: Diagnostics::default(),
        }
    }

    /// The conditions the last statement raised: `SHOW WARNINGS`,
    /// `@@warning_count`, and the count an OK packet carries.
    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    /// Replaces the diagnostics area with what a statement raised.
    ///
    /// Replaces and never appends, which is what 8.3 does: a statement that
    /// raises nothing leaves an empty area, so a `SELECT 1` between a
    /// truncating INSERT and a `SHOW WARNINGS` is why the list is empty.
    pub(crate) fn set_diagnostics(&mut self, diagnostics: Diagnostics) {
        self.diagnostics = diagnostics;
    }

    /// How many conditions the diagnostics area keeps, which MySQL spells
    /// `max_error_count`.
    ///
    /// Read straight off the override for the reason the four readers below
    /// give: it cannot change while the statement runs.
    ///
    /// The parse cannot fail for anything this engine stored:
    /// `set_system_variable` refuses a non-integer and clamps the rest into
    /// `0..=65535` first. 1024 for an absent override is the stock 8.3
    /// default, measured.
    pub fn max_error_count(&self) -> u64 {
        self.overrides
            .get("max_error_count")
            .and_then(|stored| stored.parse().ok())
            .unwrap_or(DEFAULT_MAX_ERROR_COUNT)
    }

    /// What an UPDATE reports as affected, given the rows it matched and the
    /// rows whose stored values it actually changed.
    ///
    /// One place rather than one per DML path, because the two counts are only
    /// ever different when this decision is the thing being got wrong. See
    /// `found_rows` for the measurements.
    pub fn rows_affected_by_update(&self, matched: u64, changed: u64) -> u64 {
        if self.found_rows {
            matched
        } else {
            changed
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

    /// A user variable holds 1 or 0, never a boolean.
    ///
    /// Measured on 8.3: after `SET @x = TRUE`, `JSON_ARRAY(@x)` is `[1]` and
    /// `CAST('true' AS JSON) = @x` is 0, where the bare keyword answers
    /// `[true]` and 1. A variable is somewhere to *put* a value, and the tag
    /// does not survive being put anywhere.
    pub fn set_user_variable(&mut self, name: &str, value: Value) {
        self.user_variables
            .insert(name.to_ascii_lowercase(), value.untag_bool());
    }

    /// Unset user variables read as NULL rather than erroring, as in MySQL.
    pub fn user_variable(&self, name: &str) -> Value {
        self.user_variables
            .get(&name.to_ascii_lowercase())
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// Applies a `SET` of a system variable.
    ///
    /// Nearly all of these are stored and handed straight back, so the value's
    /// text is kept as written and nothing checks it. The exceptions are the
    /// variables the engine *reads*, where a value taken unchecked is a wrong
    /// answer rather than an unread string: `EXTRACT(WEEK FROM x)` answers a
    /// different question under each of `default_week_format`'s eight values,
    /// and every quotient and every average moves with
    /// `div_precision_increment`.
    ///
    /// Every other variable is still stored unvalidated, which MySQL would
    /// refuse for most of them. That gap is about `SET` in general — #0095 —
    /// rather than about either of these, and nothing here reads those values
    /// back into an answer.
    pub fn set_system_variable(&mut self, name: &str, value: &Value) -> EngineResult<()> {
        let name = normalize_scope(name);
        let text = match name.as_str() {
            // Masking to three bits instead of clamping answers week 1 where
            // MySQL answers week 52, which is what the range is here to stop.
            "default_week_format" => clamped_integer(&name, value, 0, 7)?,
            // 30 here and `MAX_SCALE` being 30 as well is a coincidence of
            // value: this is the range MySQL documents and clamps the variable
            // to, not the scale a product or a quotient is capped at.
            "div_precision_increment" => {
                clamped_integer(&name, value, 0, crate::decimal::MAX_DIV_PRECISION_INCREMENT)?
            }
            // Clamped for the same reason the two above are: `GROUP_CONCAT`
            // now truncates at this, so a value taken unchecked is a shorter
            // answer than MySQL's rather than an unread string. Measured on
            // 8.3: 3 is stored as 4 with warning 1292, -1 is 4 as well, and
            // 18446744073709551615 is kept — the variable really does run to
            // the full `u64`, which is why `high` is that and not a smaller
            // round number.
            "group_concat_max_len" => clamped_integer(
                &name,
                value,
                MIN_GROUP_CONCAT_MAX_LEN as i128,
                u64::MAX as i128,
            )?,
            // Clamped for the same reason again: a recursive CTE stops at
            // this, so a value taken unchecked is either an answer MySQL
            // refuses or a refusal MySQL answers. Measured on 8.3: -1 is
            // stored as 0, 4294967296 as 4294967295, and 4294967295 is kept —
            // the variable is a `u32`, which is where the high bound comes
            // from.
            "cte_max_recursion_depth" => clamped_integer(&name, value, 0, u32::MAX as i128)?,
            // Clamped for the same reason again: the diagnostics area keeps
            // this many conditions, so a value taken unchecked is a longer or
            // shorter `SHOW WARNINGS` than MySQL's. Measured on 8.3: -1 is
            // stored as 0, 99999999 as 65535, and a non-integer is 1232 like
            // the four above.
            "max_error_count" => clamped_integer(&name, value, 0, MAX_MAX_ERROR_COUNT)?,
            // Read only on 8.3, and refused here rather than stored: an
            // override would shadow the diagnostics area these two are read
            // out of, so the session would report a count no statement
            // produced. Measured: `SET SESSION warning_count = 5` is 1238.
            "warning_count" | "error_count" => {
                return Err(EngineError::new(
                    1238,
                    "HY000",
                    format!("Variable '{name}' is a read only variable"),
                ))
            }
            _ => value.as_text(),
        };
        if name == "sql_mode" {
            self.config.sql_mode = text.clone();
            self.refresh_mode();
        }
        self.overrides.insert(name, text);
        Ok(())
    }

    /// The week numbering `EXTRACT(WEEK FROM x)` counts under.
    ///
    /// Read straight off the override rather than through `system_variable`,
    /// which allocates a normalised name and clones the value it finds. This
    /// is the one system variable the engine reads *per row*, and both
    /// allocations would be paid for something that cannot change while the
    /// statement runs.
    ///
    /// The parse cannot fail for anything this engine stored:
    /// `set_system_variable` above refuses a non-integer and clamps the rest
    /// into 0..=7 before it gets here. 0 for an absent override is the stock
    /// 8.3 default.
    pub fn default_week_format(&self) -> u32 {
        self.overrides
            .get("default_week_format")
            .and_then(|stored| stored.parse().ok())
            .unwrap_or(0)
    }

    /// The extra decimal places a quotient and an average gain, which MySQL
    /// spells `div_precision_increment`.
    ///
    /// Read straight off the override for the same reason as the week format
    /// above: this is asked once per arithmetic node and once per aggregate,
    /// and the normalised name and the cloned value `system_variable` builds
    /// would both be paid for something that cannot change while the statement
    /// runs.
    ///
    /// The parse cannot fail for anything this engine stored:
    /// `set_system_variable` refuses a non-integer and clamps the rest into
    /// 0..=30 before it gets here. The default for an absent override is
    /// MySQL's documented 4.
    pub fn div_precision_increment(&self) -> usize {
        self.overrides
            .get("div_precision_increment")
            .and_then(|stored| stored.parse().ok())
            .unwrap_or(crate::decimal::DEFAULT_DIV_PRECISION_INCREMENT)
    }

    /// The length `GROUP_CONCAT` truncates its answer at, in **bytes**.
    ///
    /// Bytes and not characters, measured: at 4, `GROUP_CONCAT` over a column
    /// holding `'éé'` and `'ab'` is `'éé'` — four bytes and two characters —
    /// and at 5 it is `'éé,'`.
    ///
    /// Read straight off the override for the reason the two above give: it
    /// cannot change while the statement runs, and this is asked once per
    /// group.
    ///
    /// The parse cannot fail for anything this engine stored:
    /// `set_system_variable` refuses a non-integer and clamps the rest into
    /// `4..=u64::MAX` first. 1024 for an absent override is the stock 8.3
    /// default, measured.
    pub fn group_concat_max_len(&self) -> u64 {
        self.overrides
            .get("group_concat_max_len")
            .and_then(|stored| stored.parse().ok())
            .unwrap_or(DEFAULT_GROUP_CONCAT_MAX_LEN)
    }

    /// How many iterations a recursive CTE gets before 8.3 aborts it, which
    /// MySQL spells `cte_max_recursion_depth`.
    ///
    /// Read straight off the override for the reason the three above give: it
    /// cannot change while the statement runs, and this is asked once per
    /// statement rather than once per row — so the normalised name and the
    /// cloned value `system_variable` builds would both be waste.
    ///
    /// The parse cannot fail for anything this engine stored:
    /// `set_system_variable` refuses a non-integer and clamps the rest into
    /// `0..=u32::MAX` first. 1000 for an absent override is the stock 8.3
    /// default, measured.
    pub fn cte_max_recursion_depth(&self) -> u64 {
        self.overrides
            .get("cte_max_recursion_depth")
            .and_then(|stored| stored.parse().ok())
            .unwrap_or(DEFAULT_CTE_MAX_RECURSION_DEPTH)
    }

    /// Resolves a system variable.
    ///
    /// Every driver reads several of these while connecting, and Prisma's
    /// panics rather than errors when the answer is an empty result set, so
    /// unknown names still return a value where MySQL defines one.
    pub fn system_variable(&self, name: &str) -> Option<Value> {
        let name = normalize_scope(name);
        if let Some(value) = self.overrides.get(&name) {
            return Some(Value::text(value.clone()));
        }

        let text = |s: &str| Some(Value::text(s.to_owned()));
        let int = |v: i64| Some(Value::Int(v));

        match name.as_str() {
            "version" => text(&self.config.version),
            "version_comment" => text("mysql-ultralight (test engine)"),
            // Not a MySQL variable: a real server answers 1193, which is how a
            // harness holding an instance id can tell it has reached the wrong
            // kind of server as well as the wrong process.
            "ultralight_instance" => text(&self.config.instance),
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
            "group_concat_max_len" => int(DEFAULT_GROUP_CONCAT_MAX_LEN as i64),
            // Read by a recursive CTE, so the session has to be able to answer
            // it and to carry a `SET` of it — and listed rather than left to
            // materialise on the first write, for the reason
            // `div_precision_increment` below gives. 1000 is the stock 8.3
            // default, measured.
            "cte_max_recursion_depth" => int(DEFAULT_CTE_MAX_RECURSION_DEPTH as i64),
            // `EXTRACT(WEEK FROM x)` reads this rather than taking a mode
            // argument, so the session has to be able to answer it and to
            // carry a `SET` of it. 0 is the stock 8.3 default, measured.
            "default_week_format" => int(0),
            // Read by `/` and by `AVG`, so the session has to be able to
            // answer it and to carry a `SET` of it. Listed rather than left to
            // materialise on the first write: a variable that exists only once
            // somebody has set it is worse than one that is absent, because a
            // client reading before it writes is told the variable does not
            // exist and the same client reading after is told it does.
            "div_precision_increment" => {
                int(crate::decimal::DEFAULT_DIV_PRECISION_INCREMENT as i64)
            }
            // The diagnostics area, which is session state rather than
            // configuration: `@@warning_count` counts every condition the last
            // statement raised at every level, and `@@error_count` only the
            // Error-level ones. Both are read *before* the statement reading
            // them clears the area, which is why a `SELECT @@warning_count`
            // answers what the statement before it raised.
            "warning_count" => int(self.diagnostics.count() as i64),
            "error_count" => int(self.diagnostics.error_count() as i64),
            // Listed rather than left to materialise on the first write, for
            // the reason `div_precision_increment` below gives. 1024 is the
            // stock 8.3 default, measured.
            "max_error_count" => int(DEFAULT_MAX_ERROR_COUNT as i64),
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
            "cte_max_recursion_depth",
            "default_storage_engine",
            "default_week_format",
            "div_precision_increment",
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
            "max_error_count",
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

        // `ultralight_instance` is deliberately not in that list. SHOW
        // VARIABLES is one of the outputs the differential harness compares
        // against MySQL, and a name MySQL does not have would be a divergence
        // introduced by the thing that exists to catch them. Reading it by
        // name still works, which is all the check needs.
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

/// An integer system variable's stored text, clamped to `low..=high`.
///
/// Measured on 8.3 for all four variables that use it, which behave
/// identically and differ only in their bounds. Out of range is clamped rather
/// than refused — `default_week_format = 9` is stored as 7,
/// `div_precision_increment = 31` as 30, `group_concat_max_len = 3` as 4 and
/// `cte_max_recursion_depth = 4294967296` as 4294967295, and -1 is the low
/// bound for any of them. The clamp carries warning 1292 that this engine has
/// nowhere to put. A value that is not an integer is refused with 1232, a
/// *string* spelling one included: `'3'`, `3.7`, `b'11'` and NULL are all
/// errors, while `TRUE` and `1 + 1` are taken as the numbers they are.
///
/// Shared between those four because all four were measured, and not offered
/// as a general rule: MySQL's integer variables are not uniform in what they
/// refuse and what they clamp, so a fifth one belongs here only once it has
/// been measured too. #0095 is the general question.
fn clamped_integer(name: &str, value: &Value, low: i128, high: i128) -> EngineResult<String> {
    match value {
        Value::Int(_) | Value::Unsigned(_) | Value::Bool(_) => {
            // `as_exact_int`, not `as_int`: the latter wraps a `u64` above
            // `i64::MAX` to a negative on purpose, so
            // `SET default_week_format = 18446744073709551615` clamped *down*
            // to 0 and answered week 1 for a date MySQL puts in week 52. A
            // literal that large really does arrive here as `Value::Unsigned`.
            // `i128` holds both ranges exactly.
            Ok(value
                .as_exact_int()
                .unwrap_or(0)
                .clamp(low, high)
                .to_string())
        }
        _ => Err(EngineError::new(
            1232,
            "42000",
            format!("Incorrect argument type to variable '{name}'"),
        )),
    }
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

    /// A `Session` is copied onto the stack at every level of a nested query
    /// and the engine has no stack-growth guard, so the size of this struct is
    /// a *budget* rather than a detail.
    ///
    /// Measured, on the tree before the diagnostics area was added: a `Session`
    /// was 408 bytes, and adding **eight** bytes to it overflowed the 2MB test
    /// stack in `a_deep_nest_of_scalar_subqueries_still_answers` -- a dummy
    /// `[u8; 8]` field was enough on its own, with no other change. One byte
    /// did not, because it fits the padding after `found_rows`. There was
    /// nothing left.
    ///
    /// So the diagnostics area was paid for rather than added: `transaction`
    /// went behind a `Box`, which frees 64 where the area costs 40. This
    /// asserts the result, so the next field to arrive here fails on this line
    /// with the reason beside it rather than as a stack overflow in a test
    /// three files away that says nothing about what changed.
    ///
    /// If a field genuinely needs the room, the honest fix is to stop copying
    /// a whole `Session` per recursion level, or to guard the recursion --
    /// not to raise this number.
    #[test]
    fn a_session_stays_within_its_stack_budget() {
        const BUDGET: usize = 408;
        let size = std::mem::size_of::<Session>();
        assert!(
            size <= BUDGET,
            "a Session is {size} bytes, over the {BUDGET}-byte budget; see this test"
        );
    }

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

    /// The whole point of the value is that a second server does not have it,
    /// so the two things worth asserting are that it is reported and that two
    /// servers report different ones.
    #[test]
    fn each_server_reports_its_own_instance_id() {
        let first = ServerConfig::default();
        let second = ServerConfig::default();
        assert_ne!(first.instance, second.instance);

        let s = Session::new(first.clone(), 1);
        assert_eq!(
            s.system_variable("ultralight_instance"),
            Some(Value::text(first.instance.clone()))
        );
        // Every connection to one server must answer the same, or the harness
        // would be checking the connection rather than the server.
        let other_connection = Session::new(first.clone(), 2);
        assert_eq!(
            other_connection.system_variable("@@ultralight_instance"),
            Some(Value::text(first.instance))
        );
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
        s.set_system_variable("sql_mode", &Value::text("ANSI_QUOTES"))
            .unwrap();
        assert_eq!(s.config.sql_mode, "ANSI_QUOTES");
        assert_eq!(
            s.system_variable("sql_mode"),
            Some(Value::text("ANSI_QUOTES".to_owned()))
        );
    }
}
