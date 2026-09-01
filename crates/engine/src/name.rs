//! Reading a name out of the parse tree rather than out of its rendering.
//!
//! `Ident` carries the identifier's *value* — what the tokenizer read between
//! the quotes — and its `Display` puts the quotes back. Going the other way,
//! by rendering and then trimming the quotes off again, is not the identity:
//!
//! ```text
//! written      `we``ird`     `my.table`
//! Display      `we``ird`     `my.table`
//! trimmed      we``ird       my.table   -> split on . -> my + table
//! value        we`ird        my.table
//! ```
//!
//! The first loses because `Display` *doubles* an embedded backtick and
//! trimming the outer pair cannot undo the doubling; 8.3 reports one backtick
//! everywhere it reports the name. The second loses because a caller that
//! splits the rendering on a dot cannot tell a qualifier from a dot inside a
//! name — which is how an INSERT into a table spelled ``my.table`` came to be
//! 1049 `Unknown database 'my'` against a table `CREATE` and `SELECT` were
//! both happy with.
//!
//! Every name the engine reports, stores in the image, or looks up is read
//! through here — every name that reached a parse tree, that is. The handful
//! of statements `rewrite::intercept` recognises before the parser sees them
//! have no tree to read, and read theirs out of the text by the same two
//! rules.
//!
//! This is also what makes ANSI_QUOTES a non-question: a double-quoted
//! identifier arrives with the same value as a backquoted one, so nothing
//! downstream has to know which quote was used.

use sqlparser::ast::{ObjectName, ObjectNamePart};

/// One part of a name, as it was written.
///
/// `ObjectNamePart::Function` is Snowflake's `db.IDENTIFIER('t').c` and no
/// other dialect's — `is_identifier_generating_function_name` is false for
/// MySQL — so nothing parsed here can be one. It renders rather than being
/// given an invented name: a rendering reaches the caller as a name no catalog
/// holds, which is an error, where a guess would be an answer.
pub(crate) fn part(part: &ObjectNamePart) -> String {
    match part.as_ident() {
        Some(ident) => ident.value.clone(),
        None => part.to_string(),
    }
}

/// Every part of a name, qualifier first.
pub(crate) fn parts(name: &ObjectName) -> Vec<String> {
    name.0.iter().map(part).collect()
}

/// A name joined back up as `db.table`, for the callers that hold one string.
///
/// Only for a name that is being *matched* against another built the same way,
/// never for one that is about to be split apart again: the join is lossy in
/// exactly the way this module exists to avoid, since a part may contain a dot
/// of its own.
pub(crate) fn dotted(name: &ObjectName) -> String {
    parts(name).join(".")
}

/// The last part of a name — the object itself, without its qualifier.
pub(crate) fn tail(name: &ObjectName) -> String {
    name.0.last().map(part).unwrap_or_default()
}

/// A name written back *as SQL*: wrapped in backticks, with any backtick
/// inside it doubled.
///
/// The inverse of [`part`], and the only place a name should be re-quoted.
/// MySQL's own messages are written this way — a 1452 names the parent table
/// as `` REFERENCES `pa``rent` `` — and so is `SHOW CREATE TABLE`, because
/// both are quoting SQL rather than reporting a name. Anything a client reads
/// as a *name* wants the value instead.
///
/// This exists because reading the value correctly broke the messages that
/// were relying on the old reader leaving the doubling in place: they wrapped
/// a name in backticks without escaping it, which was right only for as long
/// as the name arrived already escaped.
pub(crate) fn quote(value: &str) -> String {
    format!("`{}`", value.replace('`', "``"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::MySqlDialect;
    use sqlparser::parser::Parser;

    /// Parses `SELECT * FROM <written>` and hands back the table's name.
    fn table_name(written: &str) -> ObjectName {
        let sql = format!("SELECT * FROM {written}");
        let statements = Parser::parse_sql(&MySqlDialect {}, &sql).expect("it parses");
        let sqlparser::ast::Statement::Query(query) = &statements[0] else {
            panic!("not a query");
        };
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("not a select");
        };
        let sqlparser::ast::TableFactor::Table { name, .. } = &select.from[0].relation else {
            panic!("not a table");
        };
        name.clone()
    }

    #[test]
    fn an_embedded_backtick_is_not_doubled() {
        // The rendering is what the old readers saw, and it is one byte longer
        // than the name. Asserted here so the difference this module exists
        // for is visible in the test rather than only in the fix.
        let name = table_name("`we``ird`");
        assert_eq!(name.to_string(), "`we``ird`");
        assert_eq!(parts(&name), vec!["we`ird"]);
        assert_eq!(tail(&name), "we`ird");
    }

    #[test]
    fn a_dot_inside_a_name_is_not_a_qualifier() {
        assert_eq!(parts(&table_name("`my.table`")), vec!["my.table"]);
        assert_eq!(
            parts(&table_name("`my.db`.`my.table`")),
            vec!["my.db", "my.table"]
        );
        assert_eq!(tail(&table_name("`my.db`.`my.table`")), "my.table");
    }

    #[test]
    fn a_plain_name_is_itself() {
        assert_eq!(parts(&table_name("db.t")), vec!["db", "t"]);
        assert_eq!(dotted(&table_name("db.t")), "db.t");
        assert_eq!(dotted(&table_name("t")), "t");
    }

    #[test]
    fn a_keyword_and_a_non_ascii_name_survive() {
        assert_eq!(parts(&table_name("`select`")), vec!["select"]);
        assert_eq!(parts(&table_name("`tabké`")), vec!["tabké"]);
    }
}
