//! Statements sqlparser-rs does not accept, recognised before it sees them.
//!
//! The conformance survey found five gaps between sqlparser-rs and the SQL this
//! codebase contains, and 162 of the 173 rejected statements were two of them:
//! user variables assigned with `:=`, and the `PREPARE`/`EXECUTE`/`DEALLOCATE`
//! idiom the migrations use for conditional DDL. Both are recognised here with
//! small hand-written matchers rather than by forking the parser.

use crate::dialect::SqlMode;

/// A statement handled outside the general parser.
#[derive(Debug, Clone, PartialEq)]
pub enum Intercepted {
    /// `PREPARE name FROM <source>`, where the source is a string literal or a
    /// user variable holding one.
    Prepare { name: String, source: String },
    /// `EXECUTE name [USING @a, @b]`.
    Execute { name: String, using: Vec<String> },
    /// `DEALLOCATE PREPARE name` or `DROP PREPARE name`.
    Deallocate { name: String },
    /// `ALTER TABLE t RENAME INDEX old TO new`.
    RenameIndex {
        table: String,
        old: String,
        new: String,
    },
    /// `DROP EVENT [IF EXISTS] name`, accepted as a no-op.
    DropEvent { if_exists: bool },
}

/// Recognises a statement the parser cannot take.
pub fn intercept(sql: &str) -> Option<Intercepted> {
    let words = words_of(sql);
    let upper: Vec<String> = words.iter().map(|w| w.to_ascii_uppercase()).collect();
    let at = |i: usize| upper.get(i).map(String::as_str).unwrap_or("");

    match at(0) {
        "PREPARE" if at(2) == "FROM" && words.len() >= 4 => Some(Intercepted::Prepare {
            name: unquote(&words[1]),
            // The source may be several words when it is a quoted literal
            // containing spaces, so the remainder is rejoined.
            source: words[3..].join(" "),
        }),

        "EXECUTE" if words.len() >= 2 => {
            let using = if at(2) == "USING" {
                words[3..]
                    .iter()
                    .flat_map(|w| w.split(','))
                    .map(str::trim)
                    .filter(|w| !w.is_empty())
                    .map(unquote)
                    .collect()
            } else {
                Vec::new()
            };
            Some(Intercepted::Execute {
                name: unquote(&words[1]),
                using,
            })
        }

        "DEALLOCATE" | "DROP" if at(1) == "PREPARE" && words.len() >= 3 => {
            Some(Intercepted::Deallocate {
                name: unquote(&words[2]),
            })
        }

        "DROP" if at(1) == "EVENT" => Some(Intercepted::DropEvent {
            if_exists: at(2) == "IF" && at(3) == "EXISTS",
        }),

        // ALTER TABLE <t> RENAME INDEX <old> TO <new>, and exactly that.
        //
        // A longer statement carries further actions; handling only the rename
        // would apply it, report success, and silently drop the rest.
        "ALTER"
            if at(1) == "TABLE"
                && at(3) == "RENAME"
                && at(4) == "INDEX"
                && at(6) == "TO"
                && words.len() == 8 =>
        {
            Some(Intercepted::RenameIndex {
                table: unquote(&words[2]),
                old: unquote(&words[5]),
                new: unquote(&words[7]),
            })
        }

        _ => None,
    }
}

/// Rewrites text the parser would reject but that needs no special handling.
pub fn normalise(sql: &str, mode: SqlMode) -> String {
    let mut out = sql.to_owned();

    // `SET @x := 1` is the same statement as `SET @x = 1`; only the spelling
    // is unsupported, so it is rewritten rather than intercepted.
    if starts_with_keyword(&out, "SET") {
        out = replace_outside_literals(&out, ":=", "=");
    }

    // Under ANSI_QUOTES a double-quoted word is an identifier. Rewriting it to
    // a backtick gives the parser the same meaning without a custom dialect,
    // which sqlparser cannot honour anyway: its MySQL-specific parsing is
    // gated on the concrete dialect type, so a trait wrapper would silently
    // lose AUTO_INCREMENT and much else.
    if mode.ansi_quotes {
        out = double_quotes_to_backticks(&out);
    }

    // Under IGNORE_SPACE a function name may be separated from its `(`. The
    // parser then reads `LEFT (` as the start of a LEFT JOIN, so the space is
    // closed up. Doing this only when the mode is set keeps the default
    // behaviour untouched.
    if mode.ignore_space {
        out = close_call_spaces(&out);
    }

    out
}

/// True when the statement's first keyword matches, ignoring leading comments.
fn starts_with_keyword(sql: &str, keyword: &str) -> bool {
    words_of(sql)
        .first()
        .is_some_and(|w| w.eq_ignore_ascii_case(keyword))
}

/// A stretch of a statement, classified so rewriters can act only on code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    /// Executable text.
    Code,
    /// A string literal or quoted identifier, including its delimiters.
    Quoted,
    /// A comment, including its introducer.
    Comment,
}

/// Splits a statement into classified byte ranges.
///
/// Every rewriter here needs the same three-way distinction, and implementing
/// it separately in each produced a separate bug in each: a `"` inside a
/// comment swallowed the statement, a `--` comment absorbed the following line,
/// and an apostrophe in a comment suppressed the `:=` rewrite. There is one
/// scanner now.
fn segments(sql: &str) -> Vec<(Segment, usize, usize)> {
    let bytes = sql.as_bytes();
    let mut out: Vec<(Segment, usize, usize)> = Vec::new();
    let mut code_start = 0usize;
    let mut i = 0usize;

    let flush_code = |out: &mut Vec<(Segment, usize, usize)>, start: usize, end: usize| {
        if end > start {
            out.push((Segment::Code, start, end));
        }
    };

    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"' | b'`') => {
                flush_code(&mut out, code_start, i);
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
                out.push((Segment::Quoted, start, i));
                code_start = i;
            }
            b'#' => {
                flush_code(&mut out, code_start, i);
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                out.push((Segment::Comment, start, i));
                code_start = i;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                flush_code(&mut out, code_start, i);
                let start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                out.push((Segment::Comment, start, i));
                code_start = i;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                flush_code(&mut out, code_start, i);
                let start = i;
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                out.push((Segment::Comment, start, i));
                code_start = i;
            }
            _ => i += 1,
        }
    }

    flush_code(&mut out, code_start, sql.len());
    out
}

/// Splits into whitespace-separated words, skipping comments and keeping
/// quoted runs together.
fn words_of(sql: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for (kind, start, end) in segments(sql) {
        match kind {
            Segment::Comment => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            Segment::Quoted => current.push_str(&sql[start..end]),
            Segment::Code => {
                for ch in sql[start..end].chars() {
                    if ch.is_whitespace() || ch == ';' || ch == ',' {
                        if !current.is_empty() {
                            out.push(std::mem::take(&mut current));
                        }
                    } else {
                        current.push(ch);
                    }
                }
            }
        }
    }

    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Strips surrounding quotes or backticks from a single identifier.
///
/// Applied per dotted part, because `"db"."t"` trimmed as a whole leaves the
/// inner quotes behind and produces a database named `db"`.
fn unquote(word: &str) -> String {
    word.split('.')
        .map(|part| {
            part.trim_matches(|c| c == '`' || c == '"' || c == '\'')
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Replaces every occurrence in code, leaving literals and comments alone.
fn replace_outside_literals(sql: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    for (kind, start, end) in segments(sql) {
        let text = &sql[start..end];
        match kind {
            Segment::Code => out.push_str(&text.replace(from, to)),
            _ => out.push_str(text),
        }
    }
    out
}

/// Rewrites `"identifier"` as `` `identifier` ``.
///
/// Only quoted runs the scanner classified as such are converted, so a stray
/// double quote inside a comment cannot open an identifier that swallows the
/// rest of the statement.
fn double_quotes_to_backticks(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    for (kind, start, end) in segments(sql) {
        let text = &sql[start..end];
        if kind == Segment::Quoted && text.starts_with('"') {
            let inner = text
                .strip_prefix('"')
                .and_then(|t| t.strip_suffix('"'))
                .unwrap_or(&text[1..]);
            out.push('`');
            out.push_str(&inner.replace("\"\"", "\"").replace('`', "``"));
            out.push('`');
        } else {
            out.push_str(text);
        }
    }
    out
}

/// Removes whitespace between an identifier and a following `(`, in code only.
///
/// Doing this blind to comments deleted the newline that terminates a `--`
/// comment, commenting out everything after it.
fn close_call_spaces(sql: &str) -> String {
    let parts = segments(sql);
    let mut out = String::with_capacity(sql.len());

    for (index, (kind, start, end)) in parts.iter().enumerate() {
        let text = &sql[*start..*end];
        if *kind != Segment::Code {
            out.push_str(text);
            continue;
        }

        // A code segment that follows a comment begins with the newline that
        // ends it. Closing that up would pull the next line into the comment.
        let after_comment = index
            .checked_sub(1)
            .and_then(|p| parts.get(p))
            .is_some_and(|(k, _, _)| *k == Segment::Comment);

        let bytes = text.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if !bytes[i].is_ascii_whitespace() {
                out.push_str(&text[i..i + utf8_len(bytes[i])]);
                i += utf8_len(bytes[i]);
                continue;
            }

            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }

            // The `(` may be the first character of the next code segment, so
            // a run of whitespace at the end of this one is only closed when
            // this segment itself continues with `(`.
            let follows_paren = bytes.get(j) == Some(&b'(');
            let preceded = i > 0 || !after_comment;
            let preceded = preceded
                && out
                    .chars()
                    .next_back()
                    .is_some_and(|p| p.is_alphanumeric() || p == '_');

            if preceded && follows_paren {
                i = j;
            } else {
                out.push_str(&text[i..j]);
                i = j;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_dynamic_sql_idiom() {
        assert_eq!(
            intercept("PREPARE stmt FROM @sql"),
            Some(Intercepted::Prepare {
                name: "stmt".into(),
                source: "@sql".into()
            })
        );
        assert_eq!(
            intercept("EXECUTE stmt"),
            Some(Intercepted::Execute {
                name: "stmt".into(),
                using: Vec::new()
            })
        );
        assert_eq!(
            intercept("DEALLOCATE PREPARE stmt"),
            Some(Intercepted::Deallocate {
                name: "stmt".into()
            })
        );
    }

    #[test]
    fn execute_collects_its_using_list() {
        assert_eq!(
            intercept("EXECUTE stmt USING @a, @b"),
            Some(Intercepted::Execute {
                name: "stmt".into(),
                using: vec!["@a".into(), "@b".into()],
            })
        );
    }

    #[test]
    fn recognises_rename_index_and_drop_event() {
        assert_eq!(
            intercept("ALTER TABLE `T` RENAME INDEX `old` TO `new`"),
            Some(Intercepted::RenameIndex {
                table: "T".into(),
                old: "old".into(),
                new: "new".into(),
            })
        );
        assert_eq!(
            intercept("DROP EVENT IF EXISTS `E`"),
            Some(Intercepted::DropEvent { if_exists: true })
        );
    }

    /// An ordinary statement must fall through to the real parser.
    #[test]
    fn leaves_ordinary_statements_alone() {
        for sql in [
            "SELECT 1",
            "ALTER TABLE t RENAME TO u",
            "ALTER TABLE t ADD COLUMN a INT",
            "DROP TABLE t",
        ] {
            assert_eq!(intercept(sql), None, "{sql} should not be intercepted");
        }
    }

    #[test]
    fn colon_equals_is_rewritten_only_in_set_statements() {
        let mode = SqlMode::default();
        assert_eq!(normalise("SET @x := 1", mode), "SET @x = 1");
        // A `:=` inside a literal is data.
        assert_eq!(normalise("SET @x := ':='", mode), "SET @x = ':='");
        // Other statements are untouched.
        assert_eq!(normalise("SELECT ':=' FROM t", mode), "SELECT ':=' FROM t");
    }

    /// `LEFT (` is the LEFT() function under IGNORE_SPACE, but the parser reads
    /// LEFT as the start of a join.
    #[test]
    fn ignore_space_closes_the_gap_before_a_call() {
        let mode = SqlMode {
            ignore_space: true,
            ..Default::default()
        };
        assert_eq!(
            normalise("SELECT LEFT (MD5 (UUID ()), 21)", mode),
            "SELECT LEFT(MD5(UUID()), 21)"
        );
    }

    #[test]
    fn ignore_space_leaves_other_whitespace_alone() {
        let mode = SqlMode {
            ignore_space: true,
            ..Default::default()
        };
        assert_eq!(
            normalise("SELECT a FROM t WHERE b IN (1, 2)", mode),
            "SELECT a FROM t WHERE b IN(1, 2)"
        );
        // A space that is not before a paren survives.
        assert_eq!(normalise("SELECT a FROM t", mode), "SELECT a FROM t");
        // Without the mode, nothing changes.
        assert_eq!(
            normalise("SELECT LEFT (x, 1)", SqlMode::default()),
            "SELECT LEFT (x, 1)"
        );
    }

    /// Under ANSI_QUOTES a double-quoted word is a column, not a string.
    #[test]
    fn ansi_quotes_become_identifier_quotes() {
        let mode = SqlMode {
            ansi_quotes: true,
            ..Default::default()
        };
        assert_eq!(
            normalise(r#"SELECT "name" FROM "t""#, mode),
            "SELECT `name` FROM `t`"
        );
        // Single-quoted strings are data and must survive untouched.
        assert_eq!(
            normalise(r#"SELECT "col" FROM t WHERE x = 'a "quoted" value'"#, mode),
            "SELECT `col` FROM t WHERE x = 'a \"quoted\" value'"
        );
        // Without the mode a double-quoted word stays a string literal.
        assert_eq!(
            normalise(r#"SELECT "name""#, SqlMode::default()),
            r#"SELECT "name""#
        );
    }

    #[test]
    fn a_comment_before_the_keyword_does_not_hide_it() {
        assert_eq!(
            intercept("-- set it up\nPREPARE stmt FROM @sql"),
            Some(Intercepted::Prepare {
                name: "stmt".into(),
                source: "@sql".into()
            })
        );
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;

    const ANSI: SqlMode = SqlMode {
        ansi_quotes: true,
        pipes_as_concat: true,
        ignore_space: true,
        real_as_float: false,
        strict: true,
        only_full_group_by: true,
    };

    /// A lone double quote in a comment used to open an identifier run that
    /// consumed the rest of the statement.
    #[test]
    fn a_stray_quote_in_a_comment_does_not_swallow_the_statement() {
        assert_eq!(
            normalise("-- a 6\" pipe\nSELECT a FROM t", ANSI),
            "-- a 6\" pipe\nSELECT a FROM t"
        );
        assert_eq!(
            normalise("/* 6\" */ SELECT \"a\" FROM t", ANSI),
            "/* 6\" */ SELECT `a` FROM t"
        );
    }

    /// Collapsing whitespace blind to comments deleted the newline that ends a
    /// line comment, commenting out everything after it.
    #[test]
    fn a_line_comment_keeps_its_terminating_newline() {
        let out = normalise("INSERT INTO t -- cols\n(a, b) VALUES (1,2)", ANSI);
        assert!(out.contains("\n("), "the newline must survive: {out}");
        assert!(out.contains("VALUES(1,2)") || out.contains("VALUES (1,2)"));
    }

    /// An apostrophe in a comment opened a literal run, so the `:=` was never
    /// reached — breaking the exact idiom the migrations depend on.
    #[test]
    fn an_apostrophe_in_a_comment_does_not_suppress_the_rewrite() {
        assert_eq!(
            normalise("-- don't touch\nSET @x := 1", SqlMode::default()),
            "-- don't touch\nSET @x = 1"
        );
        assert_eq!(
            normalise("/* it's fine */ SET @x := 1", SqlMode::default()),
            "/* it's fine */ SET @x = 1"
        );
    }

    /// Intercepting a multi-action ALTER would apply the rename and silently
    /// drop everything else.
    #[test]
    fn a_multi_action_alter_is_not_intercepted() {
        assert_eq!(
            intercept("ALTER TABLE t RENAME INDEX old_ix TO new_ix, ADD COLUMN c INT"),
            None,
            "a statement with further actions must reach the real parser"
        );
        // The plain form is still recognised.
        assert!(matches!(
            intercept("ALTER TABLE t RENAME INDEX old_ix TO new_ix"),
            Some(Intercepted::RenameIndex { .. })
        ));
    }

    /// `"db"."t"` trimmed as a whole leaves the inner quotes behind.
    #[test]
    fn a_qualified_name_is_unquoted_per_part() {
        assert_eq!(unquote(r#""db"."t""#), "db.t");
        assert_eq!(unquote("`db`.`t`"), "db.t");
        assert_eq!(unquote("plain"), "plain");
    }
}
