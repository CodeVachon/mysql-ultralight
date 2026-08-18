//! Feature inventory over normalised SQL.
//!
//! Everything here runs on comment-stripped, whitespace-collapsed text. That
//! matters: the survey this replaces used line-oriented greps and missed a view
//! whose `CREATE OR REPLACE` and `VIEW` sat on separate lines.

use std::collections::BTreeMap;

/// A SQL construct worth tracking, and the phrases that identify it.
pub struct Feature {
    pub name: &'static str,
    /// Ordered phrases that must all appear, in order, in the uppercased text.
    pub phrases: &'static [&'static str],
}

macro_rules! feature {
    ($name:literal, $($phrase:literal),+) => {
        Feature { name: $name, phrases: &[$($phrase),+] }
    };
}

/// The constructs an engine has to implement, in rough order of how much work
/// each represents.
pub fn features() -> Vec<Feature> {
    vec![
        feature!("view: CREATE", "CREATE", " VIEW "),
        feature!("trigger: CREATE", "CREATE", " TRIGGER "),
        feature!("procedure: CREATE", "CREATE", " PROCEDURE "),
        feature!("function: CREATE", "CREATE", " FUNCTION "),
        feature!("event: CREATE", "CREATE", " EVENT "),
        feature!("generated column", "GENERATED ALWAYS"),
        feature!("check constraint", "CHECK ("),
        feature!("fulltext index", "FULLTEXT"),
        feature!("spatial index", "SPATIAL"),
        feature!("table partitioning", "PARTITION BY RANGE"),
        feature!("window function", " OVER ("),
        feature!("common table expression", "WITH ", " AS ("),
        feature!("recursive CTE", "WITH RECURSIVE"),
        feature!("ON DUPLICATE KEY UPDATE", "ON DUPLICATE KEY"),
        feature!("INSERT IGNORE", "INSERT IGNORE"),
        feature!("REPLACE INTO", "REPLACE INTO"),
        feature!("foreign key", "FOREIGN KEY"),
        feature!("ON DELETE CASCADE", "ON DELETE CASCADE"),
        feature!("ON DELETE SET NULL", "ON DELETE SET NULL"),
        feature!("ON UPDATE CASCADE", "ON UPDATE CASCADE"),
        feature!("collation clause", "COLLATE"),
        feature!("character set clause", "CHARACTER SET"),
        feature!("AUTO_INCREMENT", "AUTO_INCREMENT"),
        feature!("BINARY operator", "BINARY "),
        feature!("information_schema access", "INFORMATION_SCHEMA."),
        feature!("EXISTS subquery", "EXISTS ("),
        feature!("UNION", " UNION "),
        feature!("GROUP BY", "GROUP BY"),
        feature!("HAVING", " HAVING "),
        feature!("ORDER BY", "ORDER BY"),
        feature!("LEFT JOIN", "LEFT JOIN"),
        feature!("INNER JOIN", "INNER JOIN"),
        feature!("CROSS JOIN", "CROSS JOIN"),
        feature!("REGEXP", "REGEXP"),
        feature!("CASE expression", "CASE WHEN"),
        feature!("subquery in FROM", "FROM ("),
        feature!("LIMIT", " LIMIT "),
        feature!("multi-table UPDATE", "UPDATE ", " JOIN ", " SET "),
        feature!("DELETE with JOIN", "DELETE ", " JOIN "),
    ]
}

/// Counts feature occurrences in one normalised statement.
pub fn count_features(upper: &str, into: &mut BTreeMap<String, usize>) {
    for feature in features() {
        if contains_in_order(upper, feature.phrases) {
            *into.entry(feature.name.to_owned()).or_insert(0) += 1;
        }
    }
}

/// True when every phrase appears, each after the previous one.
fn contains_in_order(haystack: &str, phrases: &[&str]) -> bool {
    let mut from = 0usize;
    for phrase in phrases {
        match haystack[from..].find(phrase) {
            Some(at) => from += at + phrase.len(),
            None => return false,
        }
    }
    true
}

/// Keywords after which an identifier names a table, not a function.
const TABLE_INTRODUCERS: &[&str] = &["INTO", "FROM", "JOIN", "TABLE", "UPDATE"];

/// SQL keywords that are followed by `(` but are not function calls.
const NOT_FUNCTIONS: &[&str] = &[
    "IF",
    "IN",
    "ON",
    "AS",
    "OR",
    "AND",
    "NOT",
    "VALUES",
    "SELECT",
    "FROM",
    "WHERE",
    "SET",
    "KEY",
    "INDEX",
    "UNIQUE",
    "PRIMARY",
    "FOREIGN",
    "REFERENCES",
    "CHECK",
    "USING",
    "WHEN",
    "THEN",
    "ELSE",
    "CASE",
    "END",
    "EXISTS",
    "UNION",
    "ORDER",
    "GROUP",
    "BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "CROSS",
    "TABLE",
    "INTO",
    "INSERT",
    "UPDATE",
    "DELETE",
    "CREATE",
    "ALTER",
    "DROP",
    "ADD",
    "COLUMN",
    "CONSTRAINT",
    "DEFAULT",
    "NULL",
    "TRUE",
    "FALSE",
    "OVER",
    "PARTITION",
    "WITH",
    "RECURSIVE",
    "DISTINCT",
    "ALL",
    "ANY",
    "SOME",
    "BETWEEN",
    "LIKE",
    "IS",
    "PROCEDURE",
    "FUNCTION",
    "RETURNS",
    "BEGIN",
    "DECLARE",
    "VIEW",
    "ENUM",
    "SIGNED",
    "UNSIGNED",
    "CHARACTER",
    "COLLATE",
];

/// Extracts identifiers used in call position.
///
/// Type names such as `VARCHAR(21)` also appear this way; they are separated
/// out by the caller rather than guessed at here.
pub fn count_calls(upper: &str, into: &mut BTreeMap<String, usize>) {
    let bytes = upper.as_bytes();
    let mut i = 0usize;
    let mut previous = String::new();

    while i < bytes.len() {
        if !is_ident_start(bytes[i]) {
            i += 1;
            continue;
        }

        let start = i;
        while i < bytes.len() && is_ident_char(bytes[i]) {
            i += 1;
        }
        let name = &upper[start..i];

        // `INSERT INTO Foo (a, b)` puts a table name in call position.
        let in_table_position = TABLE_INTRODUCERS.contains(&previous.as_str());
        previous = name.to_owned();
        if in_table_position {
            continue;
        }

        // Allow whitespace before the parenthesis: IGNORE_SPACE is set in the
        // target's sql_mode, so `COUNT (x)` is legal there.
        let mut j = i;
        while j < bytes.len() && bytes[j] == b' ' {
            j += 1;
        }

        if bytes.get(j) == Some(&b'(') && !NOT_FUNCTIONS.contains(&name) {
            *into.entry(name.to_owned()).or_insert(0) += 1;
        }
    }
}

/// MySQL data type names, so type usage can be told apart from function calls.
pub const DATA_TYPES: &[&str] = &[
    "TINYINT",
    "SMALLINT",
    "MEDIUMINT",
    "INT",
    "INTEGER",
    "BIGINT",
    "DECIMAL",
    "NUMERIC",
    "FLOAT",
    "DOUBLE",
    "REAL",
    "BIT",
    "BOOLEAN",
    "BOOL",
    "DATE",
    "DATETIME",
    "TIMESTAMP",
    "TIME",
    "YEAR",
    "CHAR",
    "VARCHAR",
    "BINARY",
    "VARBINARY",
    "TINYBLOB",
    "BLOB",
    "MEDIUMBLOB",
    "LONGBLOB",
    "TINYTEXT",
    "TEXT",
    "MEDIUMTEXT",
    "LONGTEXT",
    "ENUM",
    "SET",
    "JSON",
    "GEOMETRY",
    "POINT",
    "LINESTRING",
    "POLYGON",
];

/// Counts data types where they actually appear: after a column name in a
/// definition.
///
/// Looking for `TYPE(` instead — as an earlier pass did — sees `VARCHAR(21)`
/// but silently misses every type written without a length, so `INT`,
/// `BOOLEAN`, `JSON` and `LONGTEXT` all vanish from the inventory.
pub fn count_types(upper: &str, into: &mut BTreeMap<String, usize>) {
    let bytes = upper.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }

        // Skip the quoted identifier.
        i += 1;
        while i < bytes.len() && bytes[i] != b'`' {
            i += 1;
        }
        i += 1;

        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }

        let start = i;
        while i < bytes.len() && is_ident_char(bytes[i]) {
            i += 1;
        }
        if start == i {
            continue;
        }

        let word = &upper[start..i];
        if DATA_TYPES.contains(&word) {
            *into.entry(word.to_owned()).or_insert(0) += 1;
        }
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn phrases_must_appear_in_order() {
        assert!(contains_in_order(
            "UPDATE A JOIN B SET C",
            &["UPDATE ", " JOIN ", " SET "]
        ));
        // SET before JOIN is a different statement shape.
        assert!(!contains_in_order(
            "UPDATE A SET C JOIN B",
            &["UPDATE ", " JOIN ", " SET "]
        ));
    }

    #[test]
    fn detects_a_view_whose_keywords_span_lines_once_normalised() {
        let mut counts = BTreeMap::new();
        count_features("CREATE OR REPLACE VIEW FOO AS SELECT 1", &mut counts);
        assert_eq!(counts.get("view: CREATE"), Some(&1));
    }

    #[test]
    fn call_extraction_skips_keywords_and_finds_functions() {
        let mut calls = BTreeMap::new();
        count_calls(
            "SELECT COUNT(*), ST_DISTANCE_SPHERE(A,B) FROM T WHERE X IN (1,2)",
            &mut calls,
        );
        assert_eq!(calls.get("COUNT"), Some(&1));
        assert_eq!(calls.get("ST_DISTANCE_SPHERE"), Some(&1));
        assert_eq!(calls.get("IN"), None);
    }

    #[test]
    fn types_are_counted_with_or_without_a_length() {
        let mut types = BTreeMap::new();
        count_types(
            "CREATE TABLE `T` (`A` VARCHAR(21) NOT NULL, `B` BOOLEAN, `C` JSON, `D` LONGTEXT)",
            &mut types,
        );
        assert_eq!(types.get("VARCHAR"), Some(&1));
        assert_eq!(types.get("BOOLEAN"), Some(&1));
        assert_eq!(types.get("JSON"), Some(&1));
        assert_eq!(types.get("LONGTEXT"), Some(&1));
    }

    /// `UPDATE `Foo` SET x = 1` looks exactly like a column of type SET, which
    /// is why the caller restricts this to CREATE TABLE and ALTER TABLE.
    #[test]
    fn update_set_would_be_mistaken_for_a_set_column() {
        let mut types = BTreeMap::new();
        count_types("UPDATE `FOO` SET `X` = 1", &mut types);
        assert_eq!(
            types.get("SET"),
            Some(&1),
            "documents why the caller gates this"
        );
    }

    #[test]
    fn a_quoted_identifier_not_followed_by_a_type_is_ignored() {
        let mut types = BTreeMap::new();
        count_types("SELECT `date` FROM `T`", &mut types);
        assert!(types.is_empty(), "column references are not definitions");
    }

    #[test]
    fn a_table_name_in_call_position_is_not_a_function() {
        let mut calls = BTreeMap::new();
        count_calls(
            "INSERT INTO DOCUMENTATTRIBUTEASSIGNMENT (A, B) VALUES (1, 2)",
            &mut calls,
        );
        assert_eq!(calls.get("DOCUMENTATTRIBUTEASSIGNMENT"), None);
    }

    #[test]
    fn call_extraction_tolerates_space_before_the_paren() {
        let mut calls = BTreeMap::new();
        count_calls("SELECT COUNT (*)", &mut calls);
        assert_eq!(calls.get("COUNT"), Some(&1));
    }
}
