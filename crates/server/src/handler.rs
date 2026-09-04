//! The seam between the wire protocol and whatever answers queries.
//!
//! Everything here is synchronous on purpose. The engine this server fronts is
//! in-memory with no IO in the query path, so making handlers async would add
//! a boxed future per statement and buy nothing.

use mysql_ultralight_protocol::{Capabilities, ColumnDef, ErrPacket, OkPacket, Value};

/// A result set: column metadata plus fully materialised rows.
///
/// Streaming is deliberately not modelled yet. Test-suite result sets are
/// small, and materialising keeps the executor free of lifetime plumbing until
/// there is evidence it matters.
#[derive(Debug, Clone, Default)]
pub struct ResultSet {
    pub columns: Vec<ColumnDef>,
    pub rows: Vec<Vec<Value>>,
    /// How many conditions the statement raised, for the packet that
    /// terminates the result set.
    ///
    /// A result set carries the count in its trailing EOF (or the OK that
    /// replaces one under `DEPRECATE_EOF`), which is the *only* place a SELECT
    /// can report it: there is no OK packet to put it in, and a client that
    /// does not ask `SHOW WARNINGS` learns about a condition here or nowhere.
    /// Measured on 8.3: `SELECT CAST(CAST('"x"' AS JSON) AS UNSIGNED)` comes
    /// back `1 row in set, 1 warning`.
    pub warnings: u16,
}

impl ResultSet {
    pub fn new(columns: Vec<ColumnDef>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            warnings: 0,
        }
    }

    pub fn with_rows(columns: Vec<ColumnDef>, rows: Vec<Vec<Value>>) -> Self {
        Self {
            columns,
            rows,
            warnings: 0,
        }
    }

    /// The count the terminating packet will carry. See [`Self::warnings`].
    pub fn warning_count(mut self, warnings: u16) -> Self {
        self.warnings = warnings;
        self
    }

    pub fn push(&mut self, row: Vec<Value>) {
        self.rows.push(row);
    }
}

/// What a statement produced: either rows, or a count of affected rows.
#[derive(Debug, Clone)]
pub enum Response {
    ResultSet(ResultSet),
    Ok(OkPacket),
}

impl Response {
    /// An empty success, the answer to statements like `SET`.
    pub fn ok() -> Self {
        Response::Ok(OkPacket::default())
    }

    pub fn affected(rows: u64) -> Self {
        Response::Ok(OkPacket::new(rows, 0))
    }
}

pub type HandlerResult = Result<Response, ErrPacket>;

/// What one `COM_QUERY` produced.
///
/// A multi-statement command can partly succeed: the statements before a
/// failure have already been applied. Reporting only the error would tell the
/// client nothing ran, which for a migration is a materially different claim.
#[derive(Debug, Default)]
pub struct Answered {
    pub responses: Vec<Response>,
    pub error: Option<ErrPacket>,
}

/// A statement that has been prepared but not yet executed.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub id: u32,
    pub sql: String,
    /// Placeholder count. Determines how many parameters `COM_STMT_EXECUTE`
    /// will carry, so an incorrect value desynchronises the connection.
    pub param_count: u16,
    /// Columns the statement will return, if known at prepare time.
    pub columns: Vec<ColumnDef>,
}

/// Answers commands for one connection.
///
/// One handler is constructed per connection, so implementations may hold
/// per-session state (current schema, transaction, session variables) without
/// synchronisation.
pub trait Handler: Send {
    /// The capabilities the handshake settled on, once it has.
    ///
    /// Called before the first command and never again, because the set cannot
    /// change afterwards. A handler needs it when a flag changes the *answer*
    /// rather than the packet: `CLIENT_FOUND_ROWS` decides whether an UPDATE
    /// reports the rows it matched or the rows it changed, and only the
    /// handler knows both numbers.
    fn negotiated(&mut self, capabilities: Capabilities) {
        let _ = capabilities;
    }

    /// The collation the client asked for in its handshake response.
    ///
    /// One byte of `HandshakeResponse41`, and MySQL takes the session's
    /// `collation_connection` from it — which decides what every string
    /// *literal* in every statement of the connection compares and folds
    /// under. Most clients say it once here and never again: the `mysql` crate
    /// sends 45, `utf8mb4_general_ci`, and issues no `SET NAMES` at all, while
    /// the `mysql` command line with `--default-character-set=utf8mb4` lands
    /// on `utf8mb4_0900_ai_ci`. Measured against 8.3 from both, and the two
    /// get different answers to `SELECT 'a ' = 'a'` on the same server.
    ///
    /// Called once, beside [`Handler::negotiated`] and for the same reason.
    fn connected_charset(&mut self, charset: u8) {
        let _ = charset;
    }

    /// Executes a statement arriving via `COM_QUERY`.
    fn query(&mut self, sql: &str) -> HandlerResult;

    /// Executes a `COM_QUERY` that may carry several statements.
    ///
    /// One response per statement, in order, plus the error that stopped the
    /// rest. `allow_multi` is whether the client negotiated `MULTI_STATEMENTS`;
    /// when it did not, a command with more than one statement must be refused
    /// rather than run, because the client has no way to read past the first
    /// result.
    fn query_multi(&mut self, sql: &str, allow_multi: bool) -> Answered {
        let _ = allow_multi;
        match self.query(sql) {
            Ok(response) => Answered {
                responses: vec![response],
                error: None,
            },
            Err(e) => Answered {
                responses: Vec::new(),
                error: Some(e),
            },
        }
    }

    /// Prepares a statement. `id` is assigned by the connection.
    fn prepare(&mut self, id: u32, sql: &str) -> Result<Prepared, ErrPacket>;

    /// Executes a previously prepared statement with bound parameters.
    fn execute(&mut self, stmt: &Prepared, params: &[Value]) -> HandlerResult;

    /// Handles `COM_INIT_DB` / `USE <db>`.
    fn init_db(&mut self, _db: &str) -> Result<(), ErrPacket> {
        Ok(())
    }

    /// Discards session state for `COM_RESET_CONNECTION`.
    fn reset(&mut self) {}

    /// The string reported by `COM_STATISTICS`.
    fn statistics(&self) -> String {
        "mysql-ultralight".to_owned()
    }
}
