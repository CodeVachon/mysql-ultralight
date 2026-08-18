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

    // MySQL accepts an IN list whose elements are scalar subqueries; the
    // parser reads the first as `IN (subquery)` and then fails on the comma.
    out = expand_in_subquery_lists(&out);

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

    while i < bytes.len() {
        // One primitive decides where a literal or comment ends; everything
        // that needs the distinction calls it rather than re-deriving it.
        match classify(bytes, i) {
            Some((kind, end)) => {
                if i > code_start {
                    out.push((Segment::Code, code_start, i));
                }
                out.push((kind, i, end));
                code_start = end;
                i = end;
            }
            None => i += 1,
        }
    }

    if sql.len() > code_start {
        out.push((Segment::Code, code_start, sql.len()));
    }
    out
}

/// Classifies a literal or comment starting at `i`, and where it ends.
///
/// Returns `None` when `i` is ordinary code. This is the single place that
/// knows MySQL's quoting and comment rules; implementing it per rewriter is
/// what produced three separate bugs in this module before.
fn classify(bytes: &[u8], i: usize) -> Option<(Segment, usize)> {
    match bytes[i] {
        q @ (b'\'' | b'"' | b'`') => {
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\\' && q != b'`' {
                    j += 2;
                    continue;
                }
                if bytes[j] == q {
                    // A doubled quote is an escaped quote, not a close.
                    if bytes.get(j + 1) == Some(&q) {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            Some((Segment::Quoted, (j + 1).min(bytes.len())))
        }
        b'#' => {
            let mut j = i;
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            Some((Segment::Comment, j))
        }
        b'-' if bytes.get(i + 1) == Some(&b'-') => {
            let mut j = i;
            while j < bytes.len() && bytes[j] != b'\n' {
                j += 1;
            }
            Some((Segment::Comment, j))
        }
        b'/' if bytes.get(i + 1) == Some(&b'*') => {
            let mut j = i + 2;
            while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                j += 1;
            }
            Some((Segment::Comment, (j + 2).min(bytes.len())))
        }
        _ => None,
    }
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

/// Rewrites `IN ((SELECT a), (SELECT b))` as `IN ((SELECT a) UNION ALL (SELECT b))`.
///
/// The two are equivalent for the shapes that appear in practice, and the
/// second is one both the parser and the executor already handle. Only a list
/// of two or more parenthesised SELECTs is rewritten, so `IN (SELECT ...)` and
/// ordinary value lists are untouched.
///
/// One divergence worth knowing: MySQL treats each element as a *scalar*
/// subquery and raises ER_SUBQUERY_NO_1_ROW when one returns several rows. As
/// a UNION ALL it becomes ordinary set membership, so that error is not
/// raised. Every occurrence in the target corpus carries `LIMIT 1`, so the
/// case does not arise there.
fn expand_in_subquery_lists(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0usize;

    while i < bytes.len() {
        // Literals and comments are copied verbatim; `IN` inside one is text.
        if let Some((_, end)) = classify(bytes, i) {
            out.push_str(&sql[i..end]);
            i = end;
            continue;
        }

        let is_in_keyword = bytes.len() - i >= 2
            && bytes[i..i + 2].eq_ignore_ascii_case(b"IN")
            && i.checked_sub(1).is_none_or(|p| !is_ident_byte(bytes[p]))
            && bytes.get(i + 2).is_none_or(|b| !is_ident_byte(*b));

        if !is_in_keyword {
            let len = utf8_len(bytes[i]);
            out.push_str(&sql[i..(i + len).min(sql.len())]);
            i += len;
            continue;
        }

        // Whitespace *and comments* may sit between the keyword and the list.
        let open = skip_trivia(bytes, i + 2);
        let Some(close) = (bytes.get(open) == Some(&b'('))
            .then(|| matching_paren(bytes, open))
            .flatten()
        else {
            out.push_str(&sql[i..i + 2]);
            i += 2;
            continue;
        };

        let inner = &sql[open + 1..close];
        let parts = split_top_level_commas(inner);
        if parts.len() < 2 || !parts.iter().all(|p| is_parenthesised_select(p)) {
            out.push_str(&sql[i..i + 2]);
            i += 2;
            continue;
        }

        out.push_str(&sql[i..open]);
        out.push('(');
        let branches: Vec<String> = parts
            .iter()
            // Each branch keeps its parentheses: one may end in LIMIT, and
            // `SELECT ... LIMIT 1 UNION ALL SELECT ...` binds the LIMIT to the
            // whole union and stops parsing. Branches are expanded in turn so a
            // nested list is not left behind.
            .map(|p| expand_in_subquery_lists(p.trim()))
            .collect();
        out.push_str(&branches.join(" UNION ALL "));
        out.push(')');
        i = close + 1;
    }

    out
}

/// Whether an IN-list element is a single parenthesised SELECT.
///
/// Both halves matter. Without checking that the parentheses match,
/// `(SELECT a) + (b)` looks like one; without a word boundary after the
/// keyword, so does `(selected)`. Rewriting either turns valid SQL invalid.
fn is_parenthesised_select(element: &str) -> bool {
    let trimmed = element.trim();
    let bytes = trimmed.as_bytes();

    // A leading comment is common in generated migration SQL, so it is skipped
    // rather than treated as the element not starting with a parenthesis.
    let open = skip_trivia(bytes, 0);
    if bytes.get(open) != Some(&b'(') {
        return false;
    }
    // The opening parenthesis must close at the end, or the element is an
    // expression that merely begins with one.
    let Some(close) = matching_paren(bytes, open) else {
        return false;
    };
    if skip_trivia(bytes, close + 1) != trimmed.len() {
        return false;
    }

    let inner = trimmed[open + 1..close].trim_start();
    // `get` rather than a byte slice: an element may hold multi-byte text, and
    // slicing to a fixed byte length panics when it lands mid-character.
    inner
        .get(..6)
        .is_some_and(|word| word.eq_ignore_ascii_case("SELECT"))
        && inner.as_bytes().get(6).is_none_or(|b| !is_ident_byte(*b))
}

/// Advances past whitespace and comments.
fn skip_trivia(bytes: &[u8], mut i: usize) -> usize {
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // The end of input is not trivia, and `classify` indexes the byte.
        if i >= bytes.len() {
            return i;
        }
        match classify(bytes, i) {
            Some((Segment::Comment, end)) => i = end,
            _ => return i,
        }
    }
}

/// The index of the parenthesis closing the one at `open`.
fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        if let Some(end) = classify(bytes, i).map(|(_, end)| end) {
            i = end;
            continue;
        }
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Splits on commas that are not inside parentheses, literals or comments.
fn split_top_level_commas(inner: &str) -> Vec<&str> {
    let bytes = inner.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        if let Some(end) = classify(bytes, i).map(|(_, end)| end) {
            i = end;
            continue;
        }
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&inner[start..]);
    parts
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
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

#[cfg(test)]
mod in_list_expansion {
    use super::*;

    fn expand(sql: &str) -> String {
        normalise(sql, SqlMode::default())
    }

    /// MySQL accepts an IN list of scalar subqueries; the parser reads the
    /// first as `IN (subquery)` and fails on the comma.
    #[test]
    fn a_list_of_subqueries_becomes_a_union() {
        assert_eq!(
            expand("SELECT a FROM t WHERE b IN ((SELECT x FROM u), (SELECT y FROM v))"),
            "SELECT a FROM t WHERE b IN ((SELECT x FROM u) UNION ALL (SELECT y FROM v))"
        );
    }

    #[test]
    fn three_or_more_branches_chain() {
        assert_eq!(
            expand("WHERE b IN ((SELECT 1), (SELECT 2), (SELECT 3))"),
            "WHERE b IN ((SELECT 1) UNION ALL (SELECT 2) UNION ALL (SELECT 3))"
        );
    }

    /// Shapes the parser already handles must be left exactly as they are.
    #[test]
    fn ordinary_in_lists_are_untouched() {
        for sql in [
            "WHERE b IN (1, 2, 3)",
            "WHERE b IN (SELECT x FROM u)",
            "WHERE b IN ((SELECT x FROM u))",
            "WHERE b IN ('a', 'b')",
            "WHERE b IN (f(1), g(2))",
            "WHERE b NOT IN (1, 2)",
        ] {
            assert_eq!(expand(sql), sql, "{sql} should be unchanged");
        }
    }

    #[test]
    fn not_in_is_expanded_too() {
        assert_eq!(
            expand("WHERE b NOT IN ((SELECT 1), (SELECT 2))"),
            "WHERE b NOT IN ((SELECT 1) UNION ALL (SELECT 2))"
        );
    }

    /// A branch that ends in LIMIT must keep its parentheses, or the LIMIT
    /// binds to the whole union and the statement no longer parses.
    #[test]
    fn a_branch_with_limit_keeps_its_parentheses() {
        assert_eq!(
            expand("WHERE b IN ((SELECT id FROM t LIMIT 1), (SELECT id FROM u LIMIT 1))"),
            "WHERE b IN ((SELECT id FROM t LIMIT 1) UNION ALL (SELECT id FROM u LIMIT 1))"
        );
    }

    /// A comma inside a branch belongs to that branch, not the list.
    #[test]
    fn commas_inside_a_branch_do_not_split_it() {
        assert_eq!(
            expand("WHERE b IN ((SELECT GREATEST(1, 2)), (SELECT 3))"),
            "WHERE b IN ((SELECT GREATEST(1, 2)) UNION ALL (SELECT 3))"
        );
    }

    /// `IN` inside a literal, an identifier or a comment is text.
    #[test]
    fn the_keyword_is_matched_only_as_a_word() {
        for sql in [
            "SELECT 'IN ((SELECT 1), (SELECT 2))' FROM t",
            "SELECT `IN` FROM t",
            "SELECT a FROM t -- IN ((SELECT 1), (SELECT 2))",
            "SELECT joined FROM t",
        ] {
            assert_eq!(expand(sql), sql, "{sql} should be unchanged");
        }
    }
}

#[cfg(test)]
mod in_list_review_regressions {
    use super::*;

    fn expand(sql: &str) -> String {
        normalise(sql, SqlMode::default())
    }

    /// Slicing to a fixed byte length panics when it lands mid-character, and
    /// `panic = "abort"` in the release profile takes the server with it. No
    /// SELECT is needed to reach it: an ordinary tuple list is enough.
    #[test]
    fn a_non_ascii_in_list_does_not_panic() {
        for sql in [
            "SELECT * FROM t WHERE (a,b) IN (('日本', 1), ('x', 2))",
            "WHERE b IN ((SELECT 1), ('日本語'))",
            "WHERE b IN ((日), (本))",
            "WHERE b IN ((sé), (x))",
        ] {
            // Unchanged is the right answer; not crashing is the point.
            assert_eq!(expand(sql), sql, "{sql}");
        }
    }

    /// An element that merely begins with a parenthesis is not a subquery.
    /// Rewriting it produces SQL that no longer parses.
    #[test]
    fn an_expression_starting_with_a_paren_is_not_rewritten() {
        for sql in [
            "WHERE b IN ((SELECT x)+(y), (SELECT z))",
            "WHERE b IN ((SELECT a FROM t) * (SELECT 2), (SELECT c))",
        ] {
            assert_eq!(expand(sql), sql, "{sql} must be left alone");
        }
    }

    /// The keyword test needs a word boundary, or `(selected)` looks like a
    /// subquery.
    #[test]
    fn a_word_merely_starting_with_select_is_not_a_subquery() {
        assert_eq!(
            expand("WHERE b IN ((selected), (selects))"),
            "WHERE b IN ((selected), (selects))"
        );
    }

    /// The success path used to jump past the branches it emitted, leaving a
    /// nested list to hit the very error this pass removes.
    #[test]
    fn a_nested_list_is_expanded_too() {
        assert_eq!(
            expand("WHERE b IN ((SELECT 1), (SELECT x FROM u WHERE y IN ((SELECT 2),(SELECT 3))))"),
            "WHERE b IN ((SELECT 1) UNION ALL \
             (SELECT x FROM u WHERE y IN ((SELECT 2) UNION ALL (SELECT 3))))"
        );
    }

    /// Machine-generated migration SQL carries comments freely, both before
    /// the list and inside it.
    #[test]
    fn comments_do_not_suppress_the_rewrite() {
        assert_eq!(
            expand("WHERE b IN /* x */ ((SELECT 1), (SELECT 2))"),
            "WHERE b IN /* x */ ((SELECT 1) UNION ALL (SELECT 2))"
        );
        assert_eq!(
            expand("WHERE b IN (/* x */ (SELECT 1), (SELECT 2))"),
            "WHERE b IN (/* x */ (SELECT 1) UNION ALL (SELECT 2))"
        );
    }
}
