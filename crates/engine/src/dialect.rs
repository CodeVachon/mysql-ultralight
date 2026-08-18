//! The parsing dialect, driven by `sql_mode`.
//!
//! Two of the flags the target deployment sets change what a query *means*
//! rather than how it behaves, so the mode has to reach the parser rather than
//! being consulted at execution time:
//!
//! - `ANSI_QUOTES` makes `"` an identifier quote, so `"foo"` is a column.
//! - `PIPES_AS_CONCAT` makes `||` string concatenation rather than logical OR.
//!
//! Both produce a valid parse with different semantics when unimplemented,
//! which is the failure this engine exists to avoid.
//!
//! These are applied by rewriting the statement text before parsing rather than
//! through a custom `Dialect`. sqlparser gates its MySQL-specific parsing on
//! the concrete dialect type — `dialect_of!(self is MySqlDialect | ...)`, at 94
//! sites — so a wrapper implementing the trait silently loses `AUTO_INCREMENT`,
//! index hints, and much else. See `crate::rewrite`.

/// The `sql_mode` flags that affect parsing or evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SqlMode {
    /// `"` delimits identifiers instead of strings.
    pub ansi_quotes: bool,
    /// `||` concatenates instead of meaning OR.
    pub pipes_as_concat: bool,
    /// Whitespace is allowed between a function name and its `(`.
    pub ignore_space: bool,
    /// `REAL` means `FLOAT` rather than `DOUBLE`.
    pub real_as_float: bool,
    /// Writes that would truncate or lose data are errors, not warnings.
    pub strict: bool,
}

impl SqlMode {
    /// Parses a comma-separated `sql_mode` string.
    pub fn parse(mode: &str) -> Self {
        let mut out = Self::default();
        for flag in mode.split(',') {
            match flag.trim().to_ascii_uppercase().as_str() {
                "ANSI_QUOTES" => out.ansi_quotes = true,
                "PIPES_AS_CONCAT" => out.pipes_as_concat = true,
                "IGNORE_SPACE" => out.ignore_space = true,
                "REAL_AS_FLOAT" => out.real_as_float = true,
                "STRICT_ALL_TABLES" | "STRICT_TRANS_TABLES" => out.strict = true,
                // ANSI is a compound mode that implies ANSI_QUOTES and
                // PIPES_AS_CONCAT, among others.
                "ANSI" => {
                    out.ansi_quotes = true;
                    out.pipes_as_concat = true;
                    out.ignore_space = true;
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mode the target deployment actually runs.
    const APPELLO: &str = "REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,\
                           ONLY_FULL_GROUP_BY,STRICT_ALL_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,\
                           ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION";

    #[test]
    fn parses_the_deployments_mode() {
        let mode = SqlMode::parse(APPELLO);
        assert!(mode.ansi_quotes);
        assert!(mode.pipes_as_concat);
        assert!(mode.ignore_space);
        assert!(mode.real_as_float);
        assert!(mode.strict);
    }

    #[test]
    fn the_mysql_default_sets_none_of_the_parsing_flags() {
        let mode = SqlMode::parse(
            "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,\
             ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION",
        );
        assert!(!mode.ansi_quotes);
        assert!(!mode.pipes_as_concat);
        assert!(!mode.ignore_space);
        assert!(mode.strict);
    }

    #[test]
    fn the_compound_ansi_mode_implies_the_others() {
        let mode = SqlMode::parse("ANSI");
        assert!(mode.ansi_quotes);
        assert!(mode.pipes_as_concat);
        assert!(mode.ignore_space);
    }
}
