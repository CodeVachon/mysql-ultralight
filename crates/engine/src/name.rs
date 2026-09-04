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

use sqlparser::ast::{Ident, ObjectName, ObjectNamePart};

use crate::error::{EngineError, EngineResult};

/// The longest identifier MySQL will accept, in **characters**.
///
/// Characters and not bytes, measured on 8.3: 64 two-byte `é` and 64
/// four-byte characters are both accepted as a column name and 65 of either is
/// refused, so a name may be 256 bytes long. The engine's other name limit —
/// the 255-byte cut on a *generated* column name in `exec.rs` — counts bytes,
/// which is why this one is written out rather than shared.
///
/// One number covers every kind measured: a column, a table, a database, an
/// index, a constraint, a view. A savepoint is *not* one of them — 8.3 takes a
/// 1000-character savepoint name and rolls back to it — and neither is a table
/// alias or a CTE's column name, both of which 8.3 accepts at 300 characters
/// and resolves references against in full.
pub(crate) const MAX_IDENTIFIER: usize = 64;

/// 1059 where a name is longer than MySQL will take.
///
/// For the paths that *name an object*: `CREATE TABLE`'s table and columns,
/// `CREATE DATABASE`, an index or constraint name, a view's name, and the
/// `ALTER TABLE` operations that introduce any of those.
///
/// Refusing is the point. A migration that creates a 65-character column here
/// and fails against a real server has been tested against the wrong thing,
/// which is the one outcome this engine exists to prevent.
pub(crate) fn check_identifier(value: &str) -> EngineResult<()> {
    match value.chars().count() > MAX_IDENTIFIER {
        true => Err(EngineError::identifier_too_long(value)),
        false => Ok(()),
    }
}

/// The same limit, answered 1166, for the paths where a column name is
/// *derived* rather than defined.
///
/// See `EngineError::incorrect_column_name` for which those are and why the
/// code differs.
pub(crate) fn check_column_name(value: &str) -> EngineResult<()> {
    match value.chars().count() > MAX_IDENTIFIER {
        true => Err(EngineError::incorrect_column_name(value)),
        false => Ok(()),
    }
}

/// The longest alias MySQL will report, in **bytes**.
///
/// A third limit on a third path, and the reason the two above are not enough:
/// `SELECT 1 AS <300 characters>` is not refused at all, it comes back named
/// with the first 256 bytes and a 1466 warning. Bytes rather than characters —
/// 129 two-byte `é` come back as 128 — and cut on a character boundary with
/// nothing appended: 254 `a` and a three-byte `€` come back as the 254 `a`
/// alone, at 254 bytes rather than padded out to 256.
const MAX_ALIAS_BYTES: usize = 256;

/// A select item's alias as MySQL reports it.
///
/// Every reader of an alias goes through here, because the cut is not only
/// what the client is told: MySQL cuts the alias when it stores it, so `SELECT
/// 1 AS <300> ORDER BY <300>` is 1054 there and `ORDER BY <the first 256>`
/// resolves. Cutting at the reporting site alone would report one name and
/// resolve another.
///
/// The cut carries warning 1466, whose message is not what the code's name
/// suggests and not what #0168 recorded: 8.3 answers `Leading spaces are
/// removed from name '%s'` -- ER_REMOVED_SPACES, the one condition MySQL
/// raises from `Item::set_name` for *either* reason it rewrites a name -- and
/// it quotes the alias **as written**, uncut. Measured: a 416-character alias
/// comes back as a 256-byte column name beside a message carrying all 416.
///
/// Raised once per position rather than once per call, because this is a pure
/// function and six callers ask it the same question about the same alias --
/// planning a projection, describing it, resolving an ORDER BY against it. See
/// `diagnostics::raise_once`: the key is the position and not the text,
/// because two aliases spelled identically are two conditions on 8.3.
pub(crate) fn alias(ident: &Ident) -> String {
    let value = &ident.value;
    if value.len() <= MAX_ALIAS_BYTES {
        return value.clone();
    }
    crate::diagnostics::raise_once(
        (ident.span.start.line, ident.span.start.column),
        crate::diagnostics::Level::Warning,
        1466,
        || format!("Leading spaces are removed from name '{value}'"),
    );
    let mut at = MAX_ALIAS_BYTES;
    while !value.is_char_boundary(at) {
        at -= 1;
    }
    value[..at].to_owned()
}

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

/// A column reference split into the parts MySQL's grammar allows it.
///
/// `database` is `Some` only where three parts were written, and it is the
/// part that used to be dropped: both readers of a compound identifier took
/// `parts[len - 2]` as the table and looked at nothing before it, so
/// `nodb.J2.b` asked for `J2`.`b` and answered rows out of a database the
/// statement never named. A statement written against another schema silently
/// read this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColumnParts<'a> {
    pub database: Option<&'a str>,
    pub table: Option<&'a str>,
    pub name: &'a str,
}

impl<'a> ColumnParts<'a> {
    /// A bare column name, for a caller holding one rather than a reference as
    /// it was written — a `USING` list, or a name already resolved elsewhere.
    pub(crate) fn bare(name: &'a str) -> Self {
        Self {
            database: None,
            table: None,
            name,
        }
    }

    /// The reference as it was written, for an error message.
    ///
    /// Every part joined, not just the last: 8.3 reports `Unknown column
    /// 'nodb.J2.b'`, and naming the column alone would hide which qualifier
    /// was the problem.
    pub(crate) fn written(&self) -> String {
        let mut out = String::new();
        for part in [self.database, self.table].into_iter().flatten() {
            out.push_str(part);
            out.push('.');
        }
        out.push_str(self.name);
        out
    }
}

/// Splits the identifiers of a column reference into database, table and name.
///
/// Values, not renderings, for the reason this module exists: a table spelled
/// `` `my.table` `` is one part carrying a dot, and splitting the rendered text
/// would read it as a qualifier.
///
/// `None` above three parts. MySQL's grammar has no fourth level to put one
/// at, so `SELECT w.probe.J2.b FROM J2` is 1064 on 8.3 — where this engine
/// took the last two parts and *answered rows*. A reference this cannot split
/// is one nothing in scope answers to, which is the refusal this engine gives
/// instead: 1054 rather than 1064, an error rather than a wrong answer, and
/// the difference in code is its own issue.
///
/// `None` for no parts at all, which the parser does not produce and a
/// `wrapping_sub` used to turn into a lookup of the empty name.
pub(crate) fn column_parts(parts: &[Ident]) -> Option<ColumnParts<'_>> {
    let values: Vec<&str> = parts.iter().map(|p| p.value.as_str()).collect();
    split(&values)
}

/// [`column_parts`] for a caller that already holds the values.
///
/// An assignment target arrives as an `ObjectName`, whose parts are read by
/// [`parts`] rather than borrowed, and a `USING` list holds strings too.
pub(crate) fn split<'a>(parts: &[&'a str]) -> Option<ColumnParts<'a>> {
    match parts {
        [name] => Some(ColumnParts::bare(name)),
        [table, name] => Some(ColumnParts {
            database: None,
            table: Some(table),
            name,
        }),
        [database, table, name] => Some(ColumnParts {
            database: Some(database),
            table: Some(table),
            name,
        }),
        _ => None,
    }
}

/// The database and table a `db.tbl.*` names.
///
/// The wildcard's half of [`column_parts`], and split here for the same reason:
/// the caller used to hold the qualifier as one joined string and take
/// `rsplit('.').next()` as the table, which discarded the database and could
/// not tell it from a dot inside a name either.
///
/// `None` above two parts, which the grammar has no room for.
pub(crate) fn qualifier(name: &ObjectName) -> Option<(Option<String>, String)> {
    match parts(name).as_slice() {
        [table] => Some((None, table.clone())),
        [database, table] => Some((Some(database.clone()), table.clone())),
        _ => None,
    }
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
        assert_eq!(qualifier(&name), Some((None, "we`ird".to_owned())));
    }

    #[test]
    fn a_dot_inside_a_name_is_not_a_qualifier() {
        assert_eq!(parts(&table_name("`my.table`")), vec!["my.table"]);
        assert_eq!(
            parts(&table_name("`my.db`.`my.table`")),
            vec!["my.db", "my.table"]
        );
    }

    #[test]
    fn a_plain_name_is_itself() {
        assert_eq!(parts(&table_name("db.t")), vec!["db", "t"]);
        assert_eq!(dotted(&table_name("db.t")), "db.t");
        assert_eq!(dotted(&table_name("t")), "t");
    }

    /// Parses `SELECT <written> FROM t` and hands back the reference's parts.
    fn idents(written: &str) -> Vec<Ident> {
        let sql = format!("SELECT {written} FROM t");
        let statements = Parser::parse_sql(&MySqlDialect {}, &sql).expect("it parses");
        let sqlparser::ast::Statement::Query(query) = &statements[0] else {
            panic!("not a query");
        };
        let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("not a select");
        };
        match &select.projection[0] {
            sqlparser::ast::SelectItem::UnnamedExpr(sqlparser::ast::Expr::CompoundIdentifier(
                parts,
            )) => parts.clone(),
            sqlparser::ast::SelectItem::UnnamedExpr(sqlparser::ast::Expr::Identifier(ident)) => {
                vec![ident.clone()]
            }
            other => panic!("not a column reference: {other}"),
        }
    }

    #[test]
    fn a_three_part_name_keeps_its_database() {
        let parts = idents("nodb.J2.b");
        let split = column_parts(&parts).expect("three parts split");
        assert_eq!(split.database, Some("nodb"));
        assert_eq!(split.table, Some("J2"));
        assert_eq!(split.name, "b");
        assert_eq!(split.written(), "nodb.J2.b");
    }

    #[test]
    fn a_shorter_name_has_no_database() {
        let two = idents("J2.b");
        let split = column_parts(&two).expect("two parts split");
        assert_eq!(split.database, None);
        assert_eq!(split.table, Some("J2"));
        assert_eq!(split.written(), "J2.b");

        let one = idents("b");
        let split = column_parts(&one).expect("one part splits");
        assert_eq!(split.database, None);
        assert_eq!(split.table, None);
        assert_eq!(split.written(), "b");
    }

    #[test]
    fn a_fourth_part_is_not_a_column_reference() {
        // 8.3 answers 1064 for this. What matters here is that it does not
        // split into `J2`.`b` and quietly resolve, which is what reading the
        // last two parts did.
        assert!(column_parts(&idents("w.probe.J2.b")).is_none());
        assert!(column_parts(&[]).is_none());
    }

    #[test]
    fn a_dot_inside_a_part_is_not_a_qualifier_in_a_column_reference() {
        let parts = idents("`my.db`.`my.table`.`my.column`");
        let split = column_parts(&parts).expect("three parts split");
        assert_eq!(split.database, Some("my.db"));
        assert_eq!(split.table, Some("my.table"));
        assert_eq!(split.name, "my.column");
    }

    #[test]
    fn a_wildcard_qualifier_keeps_its_database() {
        assert_eq!(
            qualifier(&table_name("nodb.J2")),
            Some((Some("nodb".to_owned()), "J2".to_owned()))
        );
        assert_eq!(qualifier(&table_name("J2")), Some((None, "J2".to_owned())));
        assert_eq!(
            qualifier(&table_name("`my.db`.`my.table`")),
            Some((Some("my.db".to_owned()), "my.table".to_owned()))
        );
    }

    #[test]
    fn a_keyword_and_a_non_ascii_name_survive() {
        assert_eq!(parts(&table_name("`select`")), vec!["select"]);
        assert_eq!(parts(&table_name("`tabké`")), vec!["tabké"]);
    }
}
