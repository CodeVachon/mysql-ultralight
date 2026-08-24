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
//!
//! `REAL_AS_FLOAT` changes a meaning too, and reaches the code by neither
//! route: the word parses the same either way and only what it *declares*
//! changes, so it is read where a type is built. `crate::exec` publishes the
//! mode for the length of a statement, which is what puts it within reach of
//! `SqlType::from_ast` without threading it through everything in between.

/// The `sql_mode` flags that affect parsing or evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SqlMode {
    /// `"` delimits identifiers instead of strings.
    pub ansi_quotes: bool,
    /// `||` concatenates instead of meaning OR.
    pub pipes_as_concat: bool,
    /// Whitespace is allowed between a function name and its `(`.
    pub ignore_space: bool,
    /// `REAL` means `FLOAT` rather than `DOUBLE`, in a declaration and in a
    /// cast alike. Read by `SqlType::from_ast`, through the statement scope
    /// `crate::exec` publishes.
    pub real_as_float: bool,
    /// Writes that would truncate or lose data are errors, not warnings.
    ///
    /// Parsed and not yet read by anything: the engine refuses an
    /// out-of-range write under every mode, so its behaviour does not turn on
    /// this. Kept rather than deleted only because the flag it comes from is
    /// real; it needs implementing or removing, like `real_as_float` did.
    pub strict: bool,
    /// A grouped query may only select columns it grouped by, or aggregates.
    pub only_full_group_by: bool,
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
                "ONLY_FULL_GROUP_BY" => out.only_full_group_by = true,
                // ANSI is a compound mode. These five are what MySQL 8.3
                // expands it to, read back from `SELECT @@sql_mode` after
                // setting it rather than taken from the manual. The list was
                // short by two — `REAL_AS_FLOAT` and `ONLY_FULL_GROUP_BY` —
                // which was invisible only while the first of those was inert.
                //
                // `TRADITIONAL` is deliberately absent: 8.3 expands it to the
                // STRICT pair and the NO_ZERO/division flags, and this struct
                // has no live field for any of them. `strict` is set by the
                // arm above and read by nothing, so an arm here would add a
                // second lie rather than remove one. See the note on `strict`.
                "ANSI" => {
                    out.real_as_float = true;
                    out.pipes_as_concat = true;
                    out.ansi_quotes = true;
                    out.ignore_space = true;
                    out.only_full_group_by = true;
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
        assert!(mode.only_full_group_by);
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

    /// Every flag MySQL 8.3 expands `ANSI` to, and only those.
    ///
    /// Measured: `SET sql_mode='ANSI'` reads back as
    /// `REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,ONLY_FULL_GROUP_BY,ANSI`.
    /// Asserting the whole struct rather than the flags it sets, because the
    /// bug this covers was two flags it *failed* to set — a test that lists
    /// only what it expects on cannot see one.
    #[test]
    fn the_compound_ansi_mode_implies_the_others() {
        assert_eq!(
            SqlMode::parse("ANSI"),
            SqlMode {
                ansi_quotes: true,
                pipes_as_concat: true,
                ignore_space: true,
                real_as_float: true,
                only_full_group_by: true,
                // Not one of ANSI's five; `TRADITIONAL` is where MySQL puts
                // the STRICT pair.
                strict: false,
            }
        );
    }
}
