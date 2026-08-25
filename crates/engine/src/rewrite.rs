//! Statements sqlparser-rs does not accept, recognised before it sees them.
//!
//! The conformance survey found five gaps between sqlparser-rs and the SQL this
//! codebase contains, and 162 of the 173 rejected statements were two of them:
//! user variables assigned with `:=`, and the `PREPARE`/`EXECUTE`/`DEALLOCATE`
//! idiom the migrations use for conditional DDL. Both are recognised here with
//! small hand-written matchers rather than by forking the parser.

use crate::dialect::SqlMode;
use sqlparser::dialect::MySqlDialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, Token, TokenWithSpan, Tokenizer};

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

/// A statement rewritten for the parser, and what the rewriting cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalised {
    /// The text to parse.
    pub sql: String,
    /// Each `%` that was written as `MOD`: its byte offset in `sql`, and the
    /// spelling it replaced.
    ///
    /// MySQL names an unaliased computed column after the text it was written
    /// as, and the executor reads that name back out of `sql` — so these are
    /// the places where `sql` no longer says what the client said. Carried
    /// rather than recovered: a rewritten `%` cannot be told from one that was
    /// typed, and `mod` is as valid a spelling as `MOD`.
    pub mod_operators: Vec<(usize, String)>,
}

/// Rewrites text the parser would reject but that needs no special handling.
pub fn normalise(sql: &str, mode: SqlMode) -> Normalised {
    let mut out = sql.to_owned();

    // `SET @x := 1` is the same statement as `SET @x = 1`; only the spelling
    // is unsupported, so it is rewritten rather than intercepted.
    if starts_with_keyword(&out, "SET") {
        out = replace_outside_literals(&out, ":=", "=");
    }

    // `DELETE a.*, b.* FROM ...` is the canonical multi-table spelling, and
    // the parser rejects the star. The `.*` carries no meaning MySQL does not
    // already take from the table name, so it is dropped.
    if starts_with_keyword(&out, "DELETE") {
        out = strip_delete_target_stars(&out);
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

    // `n MOD 3` is the same operator as `n % 3`, and only the second spelling
    // parses. Last of the passes, so the tokenizer is shown the text the
    // parser will see and the offsets it reports are offsets into the answer.
    let mut mod_operators = Vec::new();
    out = mod_operator_to_percent(&out, &mut mod_operators);

    Normalised {
        sql: out,
        mod_operators,
    }
}

/// Rewrites the infix `MOD` operator to `%`.
///
/// MySQL defines the two as one operator with one precedence, the same as `*`
/// and `DIV`, so swapping the spelling cannot regroup an expression the way
/// #0042 found `DIV` being regrouped. sqlparser's MySQL dialect implements
/// only `DIV` in its infix hook, which leaves `n MOD 3` a parse error while
/// `n % 3` and `MOD(n, 3)` both work.
///
/// `MOD(7, 2)` is a call and `7 MOD (2)` is the operator, and nothing in the
/// word itself separates them — only the tokens on either side do. Both sides
/// are checked, because either alone is wrong: without a left operand
/// `SELECT MOD(7, 2)` becomes `SELECT %(7, 2)`, and without a right one
/// `SELECT a mod FROM t`, where `mod` is a column alias, becomes a syntax
/// error. This is sqlparser's own tokenizer rather than a text search so that
/// a `MOD` inside a string, a comment or a backquoted name is not a candidate
/// at all.
///
/// Each substitution is recorded in `sites` with the word it replaced, because
/// a column named after the text it was written as has to be able to say
/// `MOD` — or `mod`, which MySQL names just as faithfully.
fn mod_operator_to_percent(sql: &str, sites: &mut Vec<(usize, String)>) -> String {
    // Nearly every statement is tokenized here for nothing, so the cheap test
    // comes first. As a *word*: `model`, `mode` and `modified_at` are ordinary
    // column names in the target schema, and a substring test would hand every
    // statement mentioning one a tokenizer pass it can never use.
    if !contains_the_word_mod(sql) {
        return sql.to_owned();
    }

    let Ok(tokens) = Tokenizer::new(&MySqlDialect {}, sql).tokenize_with_location() else {
        // Text the tokenizer refuses is text the parser will refuse, with a
        // better message than a half-applied rewrite could produce.
        return sql.to_owned();
    };

    // Whitespace and comments are not operands, and MySQL allows any amount of
    // either around an operator, so the neighbour tests read past them.
    let code: Vec<&TokenWithSpan> = tokens
        .iter()
        .filter(|t| !matches!(t.token, Token::Whitespace(_)))
        .collect();

    let mut out = String::with_capacity(sql.len());
    let mut copied = 0usize;

    for (index, token) in code.iter().enumerate() {
        // A backquoted `mod` is `Keyword::NoKeyword`, so a column of that name
        // never reaches the neighbour tests.
        if !matches!(&token.token, Token::Word(word) if word.keyword == Keyword::MOD) {
            continue;
        }
        let has_operands = index
            .checked_sub(1)
            .is_some_and(|before| ends_an_operand(&code[before].token))
            && code
                .get(index + 1)
                .is_some_and(|after| begins_an_operand(&after.token));
        if !has_operands {
            continue;
        }
        let (Some(start), Some(end)) = (
            byte_offset(sql, token.span.start),
            byte_offset(sql, token.span.end),
        ) else {
            continue;
        };
        out.push_str(&sql[copied..start]);
        sites.push((out.len(), sql[start..end].to_owned()));
        out.push('%');
        copied = end;
    }

    out.push_str(&sql[copied..]);

    // Last, because no rule on the tokens either side can finish the job.
    // `ALTER TABLE t CHANGE colx mod GEOMETRY` renames a column to `mod` and
    // has an operand on both sides of the word, exactly as `a MOD b` does —
    // and `GEOMETRY`, `SERIAL`, `FIXED` and the spatial types are not even
    // keywords sqlparser knows, so no list of them would hold. What separates
    // the two is the grammar, so the parser is asked: a rewrite that will not
    // parse is not applied, and the statement keeps the meaning it had.
    //
    // Two trades come with that. A statement broken for some *other* reason
    // reports the error its unrewritten text produces, which will point at the
    // `MOD`. And the parse here is a second one: the caller parses the same
    // text again to execute or describe it, so a statement using infix `MOD`
    // costs two full parses where every other statement costs one. That is a
    // real cost for an engine whose reason to exist is start-up and query
    // latency, and it is paid only by statements that reach here at all —
    // `contains_the_word_mod` rejects the rest on a byte scan, and `sites`
    // being empty means nothing was substituted to check.
    if !sites.is_empty() && Parser::parse_sql(&MySqlDialect {}, &out).is_err() {
        sites.clear();
        return sql.to_owned();
    }

    out
}

/// Whether `mod` appears as a whole word, ignoring case.
fn contains_the_word_mod(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    bytes.windows(3).enumerate().any(|(at, window)| {
        window.eq_ignore_ascii_case(b"mod")
            && at
                .checked_sub(1)
                .is_none_or(|before| !is_ident_byte(bytes[before]))
            && bytes.get(at + 3).is_none_or(|after| !is_ident_byte(*after))
    })
}

/// Whether a token can end an operand, and so be the left side of an operator.
///
/// The list is short on purpose. A token not on it leaves the `MOD` alone,
/// which is the parse error that already exists; being wrong the other way
/// rewrites SQL that works today into SQL that does not. A keyword MySQL also
/// accepts as a column name — `year`, `status`, and a hundred more sqlparser
/// has a `Keyword` for — is on the losing side of that trade, so `year MOD 4`
/// stays unsupported.
fn ends_an_operand(token: &Token) -> bool {
    match token {
        Token::Word(word) => {
            word.quote_style.is_some()
                || matches!(
                    word.keyword,
                    Keyword::NoKeyword | Keyword::NULL | Keyword::TRUE | Keyword::FALSE
                )
        }
        Token::RParen
        | Token::Number(..)
        | Token::Placeholder(_)
        | Token::SingleQuotedString(_)
        | Token::DoubleQuotedString(_)
        | Token::NationalStringLiteral(_)
        | Token::HexStringLiteral(_) => true,
        _ => false,
    }
}

/// Whether a token can begin an operand, and so be the right side of one.
///
/// A bare keyword is not one, even when a `(` follows it. Reading that as a
/// call — sqlparser has a `Keyword` for `ABS` and most of the rest of the
/// function library — accepts the two shapes where the word after a `MOD` is
/// a keyword introducing a parenthesised *something else*:
/// `SELECT n mod FROM (SELECT 1 AS n) x`, where `mod` is a column alias, and
/// `ALTER TABLE t CHANGE old mod VARCHAR(20)`, where it is a column being
/// renamed. Both parse today and neither would afterwards. The cost is that
/// `n MOD ABS(x)` keeps the parse error it already had; `n MOD CAST(...)` and
/// the other keywords below are spelled out instead.
///
/// `BINARY` is deliberately not among them. It starts an operand — `MOD
/// BINARY '2'` is a cast — and it is also a MySQL column type, so accepting it
/// re-opens the `CHANGE colx mod BINARY(16)` hole this list exists to close.
fn begins_an_operand(token: &Token) -> bool {
    match token {
        Token::Word(word) => {
            word.quote_style.is_some()
                || matches!(
                    word.keyword,
                    Keyword::NoKeyword
                        | Keyword::NULL
                        | Keyword::TRUE
                        | Keyword::FALSE
                        | Keyword::CASE
                        | Keyword::CAST
                        | Keyword::CONVERT
                        | Keyword::INTERVAL
                        | Keyword::EXISTS
                )
        }
        // A sign or a bitwise NOT in front of the right operand.
        Token::Minus | Token::Plus | Token::Tilde => true,
        Token::LParen
        | Token::Number(..)
        | Token::Placeholder(_)
        | Token::SingleQuotedString(_)
        | Token::DoubleQuotedString(_)
        | Token::NationalStringLiteral(_)
        | Token::HexStringLiteral(_) => true,
        _ => false,
    }
}

/// The byte index a tokenizer `Location` refers to.
///
/// Locations count lines and *characters*; every offset that indexes a
/// statement counts bytes, and any non-ASCII text ahead of the position makes
/// the two differ — `SELECT 'café'` aborted the process when they were
/// confused. There is one of these because there were two, agreeing on the
/// arithmetic and not on the edges.
///
/// `None` for the zero line or column sqlparser uses to mean an empty span,
/// rather than the subtraction underflow that panics far from its cause.
pub(crate) fn byte_offset(sql: &str, at: Location) -> Option<usize> {
    let line = usize::try_from(at.line).ok()?.checked_sub(1)?;
    let column = usize::try_from(at.column).ok()?.checked_sub(1)?;
    let start: usize = sql.split_inclusive('\n').take(line).map(str::len).sum();
    let rest = sql.get(start..)?;
    Some(
        start
            + rest
                .char_indices()
                .nth(column)
                .map_or(rest.len(), |(at, _)| at),
    )
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

/// Splits a command into its statements at top-level semicolons.
///
/// A semicolon inside a string, an identifier or a comment is not a separator,
/// which is why this is built on `segments` rather than `split(';')`. Empty
/// statements are dropped: MySQL accepts a trailing `;` and a `;;` between
/// statements, and both are common in generated migration files.
pub fn split_statements(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize;

    for (kind, from, to) in segments(sql) {
        if kind != Segment::Code {
            continue;
        }
        for i in from..to {
            if bytes[i] == b';' {
                let statement = sql[start..i].trim();
                if !statement.is_empty() {
                    out.push(statement.to_owned());
                }
                start = i + 1;
            }
        }
    }

    let tail = sql[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_owned());
    }
    out
}

/// Removes the `.*` from each target of a multi-table DELETE.
///
/// Only the text before the first `FROM` is touched, so a `SELECT a.*` inside a
/// subquery in the WHERE clause keeps its star.
fn strip_delete_target_stars(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());

    for (kind, start, end) in segments(sql) {
        if kind != Segment::Code {
            out.push_str(&sql[start..end]);
            continue;
        }

        let mut i = start;
        while i < end {
            if bytes[i] == b'.' && bytes.get(i + 1) == Some(&b'*') {
                i += 2;
                continue;
            }
            // Everything from the target list's `FROM` onward is left alone.
            if word_at(bytes, i, b"FROM") {
                out.push_str(&sql[i..]);
                return out;
            }
            let next = sql[i..end]
                .char_indices()
                .nth(1)
                .map(|(offset, _)| i + offset)
                .unwrap_or(end);
            out.push_str(&sql[i..next]);
            i = next;
        }
    }

    out
}

/// Whether `word` appears at `i` as a whole word, ignoring case.
fn word_at(bytes: &[u8], i: usize, word: &[u8]) -> bool {
    let is_word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    if i > 0 && is_word_byte(bytes[i - 1]) {
        return false;
    }
    if bytes.len() < i + word.len() {
        return false;
    }
    if !bytes[i..i + word.len()].eq_ignore_ascii_case(word) {
        return false;
    }
    bytes.get(i + word.len()).is_none_or(|b| !is_word_byte(*b))
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

    /// The text `normalise` hands the parser, which is all these look at.
    fn normalise(sql: &str, mode: SqlMode) -> String {
        super::normalise(sql, mode).sql
    }

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

    /// sqlparser only implements `DIV` infix, so `MOD` is rewritten to the `%`
    /// spelling of the same operator — but only where it *is* the operator.
    #[test]
    fn infix_mod_becomes_percent() {
        let mode = SqlMode::default();
        assert_eq!(
            normalise("SELECT n MOD 3 FROM t", mode),
            "SELECT n % 3 FROM t"
        );
        // Case and layout are the parser's business, not the rewriter's.
        assert_eq!(normalise("SELECT 8 mod 3", mode), "SELECT 8 % 3");
        assert_eq!(
            normalise("SELECT n\n  MOD 3\nFROM t", mode),
            "SELECT n\n  % 3\nFROM t"
        );
        // The right operand may be parenthesised, signed, or one of the few
        // keywords that begin a value rather than a clause.
        assert_eq!(normalise("SELECT 7 MOD (2)", mode), "SELECT 7 % (2)");
        assert_eq!(normalise("SELECT n MOD -3", mode), "SELECT n % -3");
        assert_eq!(
            normalise("SELECT 9 MOD CAST('4' AS SIGNED)", mode),
            "SELECT 9 % CAST('4' AS SIGNED)"
        );
        // A left operand that is itself a MOD call, which is the one shape
        // where both readings of the word appear in one statement.
        assert_eq!(
            normalise("SELECT MOD(7, 2) MOD 3", mode),
            "SELECT MOD(7, 2) % 3"
        );
        // Both readings, unrelated. The parser is the backstop for a rewrite
        // that will not parse, but it can only revert the *statement* — so
        // rewriting the call here would cost the operator beside it too.
        assert_eq!(
            normalise("SELECT MOD(7, 2), n MOD 3 FROM t", mode),
            "SELECT MOD(7, 2), n % 3 FROM t"
        );
    }

    /// Every `MOD` that is not the operator, which is what makes this a token
    /// pass rather than a text substitution.
    #[test]
    fn a_mod_that_is_not_the_operator_is_left_alone() {
        let mode = SqlMode::default();
        for sql in [
            // A call: the word has no left operand.
            "SELECT MOD(7, 2)",
            "SELECT a, MOD(n, 3) FROM t WHERE MOD(n, 2) = 0",
            // A column alias: the word has no right operand. A text
            // substitution turns this into a syntax error.
            "SELECT a mod FROM t",
            "SELECT id AS mod, x FROM t",
            // The same, where the clause after the alias opens with a `(`.
            // Reading a keyword before a paren as a function call accepted
            // both of these and broke them.
            "SELECT n mod FROM (SELECT 1 AS n) x",
            // A column, in a definition and in a reference.
            "CREATE TABLE t (mod VARCHAR(20))",
            "INSERT INTO t (mod) VALUES (1)",
            "ALTER TABLE t ADD COLUMN mod VARCHAR(20)",
            "SELECT `mod` FROM t",
            // A call on the right whose name sqlparser holds a keyword for.
            // Supporting it needs the lookahead that breaks the line above,
            // so this keeps the parse error it already had.
            "SELECT 8 MOD ABS(-3)",
            // `BINARY` starts an operand here and is a column type in
            // `CHANGE colx mod BINARY(16)`, so it is not on the list either.
            "SELECT 8 MOD BINARY '2'",
            // Data and commentary, not code.
            "SELECT 'a MOD b'",
            "-- n MOD 3\nSELECT 1",
        ] {
            assert_eq!(normalise(sql, mode), sql, "{sql} should be left alone");
        }
    }

    /// A column *renamed* to `mod` has an operand on both sides of the word,
    /// exactly as `a MOD b` does, so nothing local separates them. Half these
    /// type names are not keywords sqlparser knows — `GEOMETRY` and `SERIAL`
    /// tokenize as plain identifiers — which is why the guard is the parser
    /// rather than a list.
    #[test]
    fn renaming_a_column_to_mod_is_not_an_operator() {
        for kind in [
            "INT",
            "VARCHAR(20)",
            "BINARY(16)",
            "VARBINARY(16)",
            "CHAR(4)",
            "DECIMAL(5,2)",
            "FIXED(4,1)",
            "SERIAL",
            "GEOMETRY",
            "LINESTRING",
            "GEOMETRYCOLLECTION",
            "ENUM('a','b')",
            "JSON",
        ] {
            let sql = format!("ALTER TABLE t CHANGE colx mod {kind}");
            assert_eq!(normalise(&sql, SqlMode::default()), sql, "{sql}");
        }
    }

    /// The tokenizer costs more than the rest of this module put together, and
    /// the schema this engine exists for is full of `model` and `modified_at`.
    #[test]
    fn the_fast_path_matches_mod_only_as_a_word() {
        for sql in [
            "SELECT model, mode, modified_at, module FROM t",
            "SELECT amod, x_mod, mod$ FROM t",
        ] {
            assert!(!contains_the_word_mod(sql), "{sql}");
        }
        for sql in ["SELECT n MOD 3", "SELECT mod FROM t", "mod", "SELECT a-mod"] {
            assert!(contains_the_word_mod(sql), "{sql}");
        }
    }

    /// Locations from the tokenizer count characters; the offsets this module
    /// splices at count bytes, and a statement with non-ASCII text ahead of
    /// the `MOD` is where the two part company.
    #[test]
    fn a_non_ascii_statement_splices_at_the_right_byte() {
        assert_eq!(
            normalise("SELECT 'é', 8 MOD 3", SqlMode::default()),
            "SELECT 'é', 8 % 3"
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

    /// The text `normalise` hands the parser, which is all these look at.
    fn normalise(sql: &str, mode: SqlMode) -> String {
        super::normalise(sql, mode).sql
    }

    /// MySQL's `ANSI` as 8.3 expands it, so the name does not describe one
    /// mode while the fields describe another. `normalise` reads only the two
    /// text-rewriting flags, but a fixture that disagrees with `SqlMode::parse`
    /// is a second answer to a question that has one.
    const ANSI: SqlMode = SqlMode {
        ansi_quotes: true,
        pipes_as_concat: true,
        ignore_space: true,
        real_as_float: true,
        strict: false,
        only_full_group_by: true,
        no_unsigned_subtraction: false,
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
        normalise(sql, SqlMode::default()).sql
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
        normalise(sql, SqlMode::default()).sql
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
