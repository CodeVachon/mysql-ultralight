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

    /// ER_NON_UNIQ_ERROR.
    ///
    /// The clause is spelled into the message without quotes, which is how
    /// MySQL prints this one: `Column 'b' in having clause is ambiguous`,
    /// beside 1054's `Unknown column 'b' in 'having clause'`.
    pub fn ambiguous(name: &str, context: &str) -> Self {
        Self::new(
            1052,
            "23000",
            format!("Column '{name}' in {context} is ambiguous"),
        )
    }

    /// ER_AGGREGATE_ORDER_NON_AGG_QUERY.
    ///
    /// An aggregate in the ORDER BY of a query that aggregates nothing has
    /// nothing to fold over: MySQL refuses rather than reading it as one group
    /// covering the result. `SELECT s FROM t ORDER BY MAX(id)` is this on 8.3,
    /// and an aggregate inside a *subquery* there belongs to that subquery and
    /// is not one — `ORDER BY (SELECT MAX(id) FROM u)` answers.
    ///
    /// `position` is 1-based over the ORDER BY items, as 8.3 prints it.
    pub fn aggregate_order_non_aggregated(position: usize) -> Self {
        Self::new(
            3029,
            "HY000",
            format!(
                "Expression #{position} of ORDER BY contains aggregate function and applies to \
                 the result of a non-aggregated query"
            ),
        )
    }

    /// ER_FIELD_IN_ORDER_NOT_SELECT.
    ///
    /// DISTINCT settles which rows there are before ORDER BY decides what
    /// order they come in, so a sort key the select list does not carry has
    /// been thrown away by the time the sort would read it. MySQL refuses
    /// rather than picking one of the duplicates: `SELECT DISTINCT s FROM t
    /// ORDER BY id` is this on 8.3, and this engine used to answer it by
    /// sorting before the duplicates were removed.
    ///
    /// `position` is 1-based over the ORDER BY items, and `column` is the
    /// column written as `schema.table.column`, both as 8.3 prints them.
    pub fn order_not_in_select_list(position: usize, column: &str) -> Self {
        Self::new(
            3065,
            "HY000",
            format!(
                "Expression #{position} of ORDER BY clause is not in SELECT list, references \
                 column '{column}' which is not in SELECT list; this is incompatible with DISTINCT"
            ),
        )
    }

    /// ER_WINDOW_INVALID_WINDOW_FUNC_USE.
    ///
    /// A window function is computed over the result, so a clause that decides
    /// what the result *is* cannot call one. 8.3 names the function in lower
    /// case whatever the statement wrote.
    pub fn window_function_misplaced(name: &str) -> Self {
        Self::new(
            3593,
            "HY000",
            format!(
                "You cannot use the window function '{}' in this context.",
                name.to_ascii_lowercase()
            ),
        )
    }

    /// ER_WINDOW_INVALID_WINDOW_FUNC_ALIAS_USE.
    ///
    /// The same refusal reached through a select-list alias, which MySQL
    /// distinguishes because the clause never wrote the function itself.
    pub fn window_alias_misplaced(name: &str) -> Self {
        Self::new(
            3594,
            "HY000",
            format!(
                "You cannot use the alias '{name}' of an expression containing a window \
                 function in this context."
            ),
        )
    }

    /// ER_BAD_TABLE_ERROR.
    ///
    /// Distinct from `no_such_table`: 1146 is a table the catalog does not
    /// hold, and this is a qualifier the *statement* does not put in scope.
    /// `SELECT a.* FROM J2` is 1051 on 8.3 even where a table `a` exists,
    /// because the FROM clause did not name it.
    ///
    /// Named for the code rather than for the message it prints: MySQL has a
    /// second "Unknown table" error, ER_UNKNOWN_TABLE at 1109, which carries
    /// the clause as well — `Unknown table 'a' in field list`, which 8.3
    /// answers `SELECT a.b` with. That one is #0101, and this name is not
    /// the one it wants.
    pub fn bad_table(name: &str) -> Self {
        Self::new(1051, "42S02", format!("Unknown table '{name}'"))
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
