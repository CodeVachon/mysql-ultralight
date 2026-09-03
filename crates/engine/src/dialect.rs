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
    /// Read at write time, through the statement scope `crate::exec`
    /// publishes, for the same reason `real_as_float` above is: a value is
    /// written into a column several calls below the only place that holds the
    /// session. Without it a string wider than the column declares is cut to
    /// the width and an out-of-range number is clamped to the end of the
    /// range, which is what 8.3 does — 300 into a TINYINT is 127 there under
    /// `sql_mode=''`, and 1264 under the default mode.
    ///
    /// Two things it does *not* yet reach, both of which stay errors under
    /// every mode: a value that is not a number at all going into a numeric
    /// column, which 8.3 stores as 0 with warning 1366, and a FLOAT whose
    /// declared `(M,D)` cannot hold it. Each is a wrong stored value rather
    /// than a missing one, so refusing is the safer of the two gaps.
    pub strict: bool,
    /// A grouped query may only select columns it grouped by, or aggregates.
    pub only_full_group_by: bool,
    /// `-` between integers is signed even when an operand is UNSIGNED.
    ///
    /// Read at evaluation time, through the statement scope `crate::exec`
    /// publishes, for the same reason `real_as_float` above is: the operands
    /// of a subtraction are several recursions below the only place that holds
    /// the session.
    ///
    /// It is the *operator* and not the operand kind that changes, which is
    /// measured rather than taken from the name: under this mode `iu - 100` is
    /// -93 and reported signed, while `iu + -100` and `iu * -1` still raise
    /// 1690 and `iu + 1` is still UNSIGNED.
    pub no_unsigned_subtraction: bool,
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
                "NO_UNSIGNED_SUBTRACTION" => out.no_unsigned_subtraction = true,
                // ANSI is a compound mode. These five are what MySQL 8.3
                // expands it to, read back from `SELECT @@sql_mode` after
                // setting it rather than taken from the manual. The list was
                // short by two — `REAL_AS_FLOAT` and `ONLY_FULL_GROUP_BY` —
                // which was invisible only while the first of those was inert.
                "ANSI" => {
                    out.real_as_float = true;
                    out.pipes_as_concat = true;
                    out.ansi_quotes = true;
                    out.ignore_space = true;
                    out.only_full_group_by = true;
                }
                // The other compound mode, and the one that turns strictness
                // on. 8.3 reads `SET sql_mode='TRADITIONAL'` back as
                // `STRICT_TRANS_TABLES,STRICT_ALL_TABLES,NO_ZERO_IN_DATE,
                // NO_ZERO_DATE,ERROR_FOR_DIVISION_BY_ZERO,TRADITIONAL,
                // NO_ENGINE_SUBSTITUTION` — measured, not taken from the
                // manual — and `strict` is the only one of those seven this
                // struct has a field for. The four it does not are a gap of
                // their own; leaving the whole arm out was only defensible
                // while `strict` was inert, and under it `'abcdef'` into a
                // `VARCHAR(3)` stored `abc` where 8.3 answers 1406.
                "TRADITIONAL" => out.strict = true,
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
                no_unsigned_subtraction: false,
            }
        );
    }
}
