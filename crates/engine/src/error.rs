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

    /// ER_CANT_DROP_FIELD_OR_KEY.
    ///
    /// The one answer MySQL gives an `ALTER TABLE ... DROP` that names
    /// something the table does not carry, whichever name space the operation
    /// reads: `DROP COLUMN`, `DROP INDEX`, `DROP KEY`, `DROP FOREIGN KEY` and
    /// `DROP PRIMARY KEY` are all 1091 on 8.3, as is the standalone
    /// `DROP INDEX nosuch ON t`.
    ///
    /// Not 1054, which is what a *reference* to a missing column is. 8.3 keeps
    /// the two apart on the same statement: `ALTER TABLE t MODIFY nosuch INT`
    /// and `RENAME COLUMN nosuch TO z` are 1054 `Unknown column 'nosuch' in
    /// 't'` there, while `DROP nosuch` is this.
    ///
    /// The name spaces do not leak into one another either, so the shared code
    /// is not a shared lookup: with `INDEX ix (a)` on the table, 8.3 answers
    /// both `DROP COLUMN ix` and `DROP INDEX a` with 1091.
    ///
    /// `DROP PRIMARY KEY` on a table with no primary key reports the literal
    /// `PRIMARY`, which is the name MySQL keeps a primary key's index under.
    ///
    /// `DROP KEY` is 1091 on 8.3 and never reaches here: sqlparser reads no
    /// `KEY` spelling of the operation, so this engine answers that one 1064.
    /// A parser gap and not a missing code — worth knowing before someone
    /// concludes from the tests that the spelling is covered.
    pub fn cant_drop(name: &str) -> Self {
        Self::new(
            1091,
            "42000",
            format!("Can't DROP '{name}'; check that column/key exists"),
        )
    }

    /// ER_CONSTRAINT_NOT_FOUND.
    ///
    /// `DROP CONSTRAINT` misses with its own code rather than the 1091 the
    /// drops beside it share, because it reads a different name space: the one
    /// `information_schema.TABLE_CONSTRAINTS` lists, which is primary key,
    /// unique, foreign key and check — and no ordinary non-unique index.
    /// Measured on 8.3, where a table carrying `INDEX ix (a)` answers
    /// `DROP CONSTRAINT ix` with this and leaves the index standing, though
    /// `DROP INDEX ix` would take it.
    ///
    /// A *unique* index is in it, under every spelling — `CONSTRAINT uq UNIQUE
    /// (a)`, `UNIQUE INDEX uq (a)`, `UNIQUE KEY uq (a)` and the index a
    /// column-level `a INT UNIQUE` creates under the column's own name are all
    /// dropped by `DROP CONSTRAINT` on 8.3. So is the primary key, under the
    /// literal `PRIMARY` and never under the constraint name it was written
    /// with: `CONSTRAINT mypk PRIMARY KEY (a)` then `DROP CONSTRAINT mypk` is
    /// this, while `` DROP CONSTRAINT `PRIMARY` `` on the same table succeeds.
    ///
    /// The trailing full stop is MySQL's, and the message has none of the
    /// advice 1091's carries.
    ///
    /// Not the code for `DROP CHECK`, which is a fourth answer again: 8.3
    /// gives that one 3821 `Check constraint 'nosuch' is not found in the
    /// table.` even though a check is in the name space this reads. sqlparser
    /// does not read that operation either, so nothing here can reach 3821 —
    /// a gap, and not one to paper over by widening this.
    pub fn constraint_not_found(name: &str) -> Self {
        Self::new(
            3940,
            "HY000",
            format!("Constraint '{name}' does not exist."),
        )
    }

    /// 3939: one name, more than one kind of constraint.
    ///
    /// Named by its code rather than by a symbol, because the code and the
    /// message are what was measured — the 8.3 image carries no error-symbol
    /// table to read the name out of, and guessing one would be a claim
    /// nothing here checked.
    ///
    /// The name spaces `DROP CONSTRAINT` reads across are separate from each
    /// other, so one name can be in two of them at once and the statement no
    /// longer says which is meant. 8.3 refuses rather than dropping both:
    /// `CONSTRAINT n UNIQUE (a), CONSTRAINT n CHECK (b > 0)` and `CONSTRAINT n
    /// UNIQUE (a), CONSTRAINT n FOREIGN KEY (a) REFERENCES R(id)` are both
    /// this, as is a table with a primary key beside a check written as
    /// `` CONSTRAINT `PRIMARY` CHECK (...) ``. The advice is to reach for
    /// `DROP INDEX`, `DROP FOREIGN KEY` or `DROP CHECK` instead, which each
    /// read one space.
    ///
    /// Two of a *kind* cannot happen and this is not about that: two indexes
    /// of one name are 1061, and two foreign keys are 1826.
    pub fn multiple_constraints(name: &str) -> Self {
        Self::new(
            3939,
            "HY000",
            format!(
                "Table has multiple constraints with the name '{name}'. \
                 Please use constraint specific 'DROP' clause."
            ),
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

    /// ER_UNKNOWN_TABLE, for a multi-table DELETE target the FROM clause does
    /// not name.
    ///
    /// The third "Unknown table" and the second code: 1146 is a table the
    /// catalog does not hold and 1051 a qualifier the statement does not put
    /// in scope, and this is 8.3's answer for `DELETE nodb.A FROM A JOIN B ON
    /// A.id = B.id`, where the catalog does hold an `A` and the FROM clause
    /// does name it — under a different database. It is the same refusal for a
    /// target written with no database at all: `DELETE Nope FROM A JOIN B` is
    /// 1109 there too, not 1146.
    ///
    /// `name` is the table part alone, without the database written in front
    /// of it, which is how 8.3 prints it: `DELETE nodb.A FROM A` reports
    /// `Unknown table 'A' in MULTI DELETE`.
    pub fn unknown_multi_delete_target(name: &str) -> Self {
        Self::new(
            1109,
            "42S02",
            format!("Unknown table '{name}' in MULTI DELETE"),
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

    /// ER_DUP_KEYNAME.
    ///
    /// An index name is scoped to its table — the same name on two tables is
    /// legal, unlike a foreign key's or a check's, which are scoped to the
    /// schema and refused with 1826 and 3822 instead. `name` is spelled as the
    /// statement wrote it rather than as the table stored it: on 8.3,
    /// `INDEX i (a), INDEX I (a)` reports `Duplicate key name 'I'`.
    pub fn duplicate_key_name(name: &str) -> Self {
        Self::new(1061, "42000", format!("Duplicate key name '{name}'"))
    }

    /// ER_BAD_NULL_ERROR.
    pub fn null_not_allowed(column: &str) -> Self {
        Self::new(1048, "23000", format!("Column '{column}' cannot be null"))
    }

    /// ER_TOO_LONG_IDENT.
    ///
    /// MySQL's answer for an identifier past 64 characters on every path that
    /// *names* something — a table, a database, a column being defined, an
    /// index, a constraint, a view. `name::check_identifier` is where the
    /// limit itself is measured and recorded.
    pub fn identifier_too_long(name: &str) -> Self {
        Self::new(
            1059,
            "42000",
            format!("Identifier name '{}' is too long", quoted_in_message(name)),
        )
    }

    /// ER_WRONG_COLUMN_NAME.
    ///
    /// The same limit reached down a different path. MySQL answers 1059 where
    /// a column is being *defined* and 1166 where a column name is being
    /// *derived* — `ALTER TABLE t RENAME COLUMN`, a view's column list, an
    /// alias inside a view's definition, and the names `CREATE TABLE ... AS
    /// SELECT` takes from its query. Measured on 8.3; the two codes are not
    /// interchangeable and a client branching on them would see the
    /// difference.
    pub fn incorrect_column_name(name: &str) -> Self {
        Self::new(
            1166,
            "42000",
            format!("Incorrect column name '{}'", quoted_in_message(name)),
        )
    }

    /// ER_PARSE_ERROR.
    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(1064, "42000", message)
    }
}

/// A name as MySQL quotes it into an error message: cut at 100 bytes.
///
/// Both 1059 and 1166 hold their name in a `%-.100s`, so the name that comes
/// back is never the one that was written past that. Measured on 8.3: a
/// 300-character name is reported as 100 characters, and 50 two-byte
/// characters — exactly 100 bytes — come back whole.
///
/// A character straddling the cut becomes one `?` **per byte** it would have
/// occupied, so a truncated name is always exactly 100 bytes. That is measured
/// rather than assumed, and it is not the character-boundary cut used
/// elsewhere in this engine: 98 `a` followed by a three-byte `€` comes back as
/// 98 `a` and *two* question marks, where 99 `a` and the same `€` come back
/// with one.
fn quoted_in_message(name: &str) -> String {
    const MAX_MESSAGE_BYTES: usize = 100;
    if name.len() <= MAX_MESSAGE_BYTES {
        return name.to_owned();
    }
    let mut at = MAX_MESSAGE_BYTES;
    while !name.is_char_boundary(at) {
        at -= 1;
    }
    let mut out = String::with_capacity(MAX_MESSAGE_BYTES);
    out.push_str(&name[..at]);
    for _ in at..MAX_MESSAGE_BYTES {
        out.push('?');
    }
    out
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
