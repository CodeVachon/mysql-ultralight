//! Splitting a SQL file into individual statements.
//!
//! Parsing a whole file at once is no good for this tool: one unsupported
//! statement would hide every other statement in the file. Splitting first
//! means a parse failure costs exactly one statement of coverage, and the
//! failures themselves become the most useful part of the report.
//!
//! Semicolons inside string literals, quoted identifiers and comments are not
//! statement terminators, so this has to track quoting rather than split on
//! the character.

/// One statement, with the line it started on so failures can be located.
#[derive(Debug, Clone)]
pub struct Statement {
    pub sql: String,
    pub line: usize,
}

/// Splits SQL into statements, discarding comments and blank fragments.
pub fn split(sql: &str) -> Vec<Statement> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut line = 1usize;
    let mut start_line = 1usize;
    let mut i = 0usize;

    // Tracks whether anything other than whitespace has been seen since the
    // last terminator, so `start_line` points at real SQL rather than the
    // blank lines before it.
    let mut seen_content = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                line += 1;
                i += 1;
            }
            q @ (b'\'' | b'"' | b'`') => {
                // A statement may open with a quoted identifier, so this arm
                // has to claim the start line just as the default arm does.
                if !seen_content {
                    seen_content = true;
                    start_line = line;
                }
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    // Backslash escapes apply inside string literals but not
                    // inside backtick-quoted identifiers.
                    if bytes[i] == b'\\' && q != b'`' {
                        // The escaped byte may itself be a newline, and
                        // skipping it uncounted makes every later statement in
                        // the file report a line number that is too low.
                        if bytes.get(i + 1) == Some(&b'\n') {
                            line += 1;
                        }
                        i += 2;
                        continue;
                    }
                    if bytes[i] == q {
                        // A doubled quote is an escaped quote, not a close.
                        if bytes.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
            }
            b';' => {
                push(&mut out, &sql[start..i], start_line);
                i += 1;
                start = i;
                start_line = line;
                seen_content = false;
            }
            c => {
                if !seen_content && !c.is_ascii_whitespace() {
                    seen_content = true;
                    start_line = line;
                }
                i += 1;
            }
        }
    }

    // A trailing statement without a terminating semicolon still counts.
    if start < sql.len() {
        push(&mut out, &sql[start..], start_line);
    }

    out
}

fn push(out: &mut Vec<Statement>, raw: &str, line: usize) {
    let sql = strip_comments(raw);
    if !sql.trim().is_empty() {
        out.push(Statement {
            sql: sql.trim().to_owned(),
            line,
        });
    }
}

/// Removes comments while preserving anything inside quotes.
pub fn strip_comments(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"' | b'`') => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && q != b'`' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == q {
                        if bytes.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
                out.push_str(&sql[start..i]);
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                out.push(' ');
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                out.push(' ');
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                out.push(' ');
            }
            _ => {
                // Push the whole UTF-8 character, not the byte.
                let ch_len = utf8_len(bytes[i]);
                out.push_str(&sql[i..(i + ch_len).min(sql.len())]);
                i += ch_len;
            }
        }
    }

    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Blanks the contents of string literals, keeping the quotes.
///
/// Data is not code: `'rgb(255,0,0)'` in an INSERT is a colour, not a call to a
/// function named RGB, and counting it as one pollutes the inventory.
pub fn mask_string_literals(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            // Backtick-quoted identifiers are copied through untouched. They
            // may legitimately contain an apostrophe, which would otherwise be
            // read as opening a string literal.
            b'`' => {
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != b'`' {
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
                out.push_str(&sql[start..i]);
            }
            q @ (b'\'' | b'"') => {
                out.push(q as char);
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == q {
                        if bytes.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                out.push(q as char);
                i += 1;
            }
            _ => {
                let ch_len = utf8_len(bytes[i]);
                out.push_str(&sql[i..(i + ch_len).min(sql.len())]);
                i += ch_len;
            }
        }
    }

    out
}

/// Collapses all whitespace, so patterns match across the line breaks that
/// defeated the earlier grep-based survey.
pub fn normalize(sql: &str) -> String {
    strip_comments(sql)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_real_terminators_only() {
        let sql = "SELECT 1; SELECT ';'; SELECT `a;b`;";
        let stmts = split(sql);
        assert_eq!(stmts.len(), 3);
        assert_eq!(stmts[1].sql, "SELECT ';'");
        assert_eq!(stmts[2].sql, "SELECT `a;b`");
    }

    #[test]
    fn discards_comments_but_keeps_quoted_lookalikes() {
        let stmts = split("SELECT 1 -- ; not a split\n; SELECT '-- literal'");
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[1].sql, "SELECT '-- literal'");
    }

    #[test]
    fn reports_the_line_a_statement_starts_on() {
        let stmts = split("SELECT 1;\n\n\nSELECT 2;");
        assert_eq!(stmts[0].line, 1);
        assert_eq!(stmts[1].line, 4);
    }

    /// The failure that defeated the grep survey: a statement wrapping across
    /// lines. Normalising must bring the keywords back together.
    #[test]
    fn normalize_joins_statements_split_across_lines() {
        let sql = "CREATE OR REPLACE\nVIEW RecipientListFeed AS\nSELECT 1";
        assert_eq!(
            normalize(sql),
            "CREATE OR REPLACE VIEW RecipientListFeed AS SELECT 1"
        );
    }

    #[test]
    fn string_contents_are_blanked_but_the_quotes_remain() {
        assert_eq!(
            mask_string_literals("SELECT 'rgb(1,2)' , x"),
            "SELECT '' , x"
        );
        assert_eq!(mask_string_literals("SELECT 'it''s'"), "SELECT ''");
    }

    #[test]
    fn a_trailing_statement_without_a_semicolon_counts() {
        assert_eq!(split("SELECT 1;\nSELECT 2").len(), 2);
    }

    /// An apostrophe inside a quoted identifier must not start a literal, or
    /// the rest of the statement is swallowed and its functions go uncounted.
    #[test]
    fn quoted_identifiers_survive_masking() {
        assert_eq!(
            mask_string_literals("SELECT `it's ok`, 'x' FROM t"),
            "SELECT `it's ok`, '' FROM t"
        );
    }

    #[test]
    fn a_statement_opening_with_a_quoted_identifier_reports_its_own_line() {
        let stmts = split("INSERT INTO t VALUES (1);\n\n`weird`.x;");
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[1].line, 3);
    }

    /// A backslash-escaped newline inside a literal still advances the line
    /// counter; skipping it uncounted makes the drift accumulate down the file.
    #[test]
    fn an_escaped_newline_inside_a_literal_is_still_counted() {
        let stmts = split("SELECT 'a\\\nb';\nSELECT 2;");
        assert_eq!(stmts[1].line, 3);
    }
}
