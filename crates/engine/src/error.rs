//! Engine errors, carrying the MySQL error code and SQLSTATE clients branch on.

use mysql_ultralight_protocol::ErrPacket;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError {
    pub code: u16,
    pub sql_state: String,
    pub message: String,
}

pub type EngineResult<T> = Result<T, EngineError>;

impl EngineError {
    pub fn new(code: u16, sql_state: &str, message: impl Into<String>) -> Self {
        Self {
            code,
            sql_state: sql_state.to_owned(),
            message: message.into(),
        }
    }

    /// ER_NO_SUCH_TABLE.
    pub fn no_such_table(name: &str) -> Self {
        Self::new(1146, "42S02", format!("Table '{name}' doesn't exist"))
    }

    /// ER_BAD_FIELD_ERROR.
    pub fn unknown_column(name: &str, context: &str) -> Self {
        Self::new(
            1054,
            "42S22",
            format!("Unknown column '{name}' in '{context}'"),
        )
    }

    /// ER_TABLE_EXISTS_ERROR.
    pub fn table_exists(name: &str) -> Self {
        Self::new(1050, "42S01", format!("Table '{name}' already exists"))
    }

    /// ER_NO_DB_ERROR.
    pub fn no_database() -> Self {
        Self::new(1046, "3D000", "No database selected")
    }

    /// ER_BAD_DB_ERROR.
    pub fn unknown_database(name: &str) -> Self {
        Self::new(1049, "42000", format!("Unknown database '{name}'"))
    }

    /// ER_DUP_ENTRY.
    pub fn duplicate_entry(value: &str, key: &str) -> Self {
        Self::new(
            1062,
            "23000",
            format!("Duplicate entry '{value}' for key '{key}'"),
        )
    }

    /// ER_BAD_NULL_ERROR.
    pub fn null_not_allowed(column: &str) -> Self {
        Self::new(1048, "23000", format!("Column '{column}' cannot be null"))
    }

    /// ER_PARSE_ERROR.
    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(1064, "42000", message)
    }
}

/// ER_NOT_SUPPORTED_YET.
///
/// The engine's honest answer for anything unimplemented. Returning a plausible
/// result instead would leave a test suite unable to tell that the feature is
/// missing, which is the one failure mode a test database must not have.
pub fn unsupported(what: impl std::fmt::Display) -> EngineError {
    EngineError::new(
        1235,
        "0A000",
        format!("mysql-ultralight does not yet support {what}"),
    )
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ERROR {} ({}): {}",
            self.code, self.sql_state, self.message
        )
    }
}

impl std::error::Error for EngineError {}

impl From<EngineError> for ErrPacket {
    fn from(e: EngineError) -> Self {
        ErrPacket::new(e.code, &e.sql_state, e.message)
    }
}
