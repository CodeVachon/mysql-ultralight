//! Statements sqlparser-rs does not accept, recognised before it sees them.
//!
//! The conformance survey found five gaps between sqlparser-rs and the SQL this
//! codebase contains, and 162 of the 173 rejected statements were two of them:
//! user variables assigned with `:=`, and the `PREPARE`/`EXECUTE`/`DEALLOCATE`
//! idiom the migrations use for conditional DDL. Both are recognised here with
//! small hand-written matchers rather than by forking the parser.

use crate::dialect::SqlMode;
use crate::temporal::Compound;
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
        /// The table's name, part by part. Not joined: a part may hold a dot
        /// of its own, and nothing downstream could split it back out.
        table: Vec<String>,
        old: String,
        new: String,
    },
    /// `DROP EVENT [IF EXISTS] name`, accepted as a no-op.
    DropEvent { if_exists: bool },
    /// `SHOW WARNINGS`, `SHOW ERRORS` and the two `SHOW COUNT(*)` forms.
    ShowDiagnostics(ShowDiagnostics),
}

/// Which question one of the diagnostics statements asks.
///
/// Intercepted rather than parsed because sqlparser has no node for any of
/// them: `SHOW WARNINGS` reaches it as `ShowVariable { variable: [WARNINGS] }`,
/// `SHOW WARNINGS LIMIT 2` does not parse at all, and neither does `SHOW
/// COUNT(*) WARNINGS`. Reading them here is the same trade `PREPARE` and
/// `EXECUTE` make above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowDiagnostics {
    /// `SHOW ERRORS`, which shows only the Error-level conditions. The count
    /// forms split the same way: `SHOW COUNT(*) ERRORS` is `@@error_count`.
    pub errors_only: bool,
    /// `SHOW COUNT(*) ...`: how many, rather than which.
    pub count_only: bool,
    /// `LIMIT [offset,] row_count`, which 8.3 accepts on the list forms.
    pub limit: Option<(u64, u64)>,
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

        // The diagnostics statements. Anything spelled otherwise is left to
        // the parser, which is what makes a form this does not read a 1235
        // rather than a wrong answer: `SHOW WARNINGS LIMIT x` is refused here
        // instead of being taken as an unlimited `SHOW WARNINGS`.
        "SHOW" if matches!(at(1), "WARNINGS" | "ERRORS") => {
            let rest: Vec<&str> = upper[2..].iter().map(String::as_str).collect();
            let limit = match rest.as_slice() {
                [] => None,
                // `words_of` splits on a comma as well as on whitespace, so
                // `LIMIT 1,2` and `LIMIT 1, 2` arrive the same way.
                ["LIMIT", count] => Some((0, count.parse().ok()?)),
                ["LIMIT", offset, count] => Some((offset.parse().ok()?, count.parse().ok()?)),
                _ => return None,
            };
            Some(Intercepted::ShowDiagnostics(ShowDiagnostics {
                errors_only: at(1) == "ERRORS",
                count_only: false,
                limit,
            }))
        }

        // `SHOW COUNT(*) WARNINGS`, whose middle is rejoined rather than
        // matched word for word: the parentheses are not separators here, so
        // `COUNT( * )` arrives as three words and `COUNT(*)` as one.
        "SHOW"
            if words.len() >= 3
                && matches!(upper[upper.len() - 1].as_str(), "WARNINGS" | "ERRORS")
                && upper[1..upper.len() - 1].concat() == "COUNT(*)" =>
        {
            Some(Intercepted::ShowDiagnostics(ShowDiagnostics {
                errors_only: upper[upper.len() - 1] == "ERRORS",
                count_only: true,
                limit: None,
            }))
        }

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
                table: unquote_parts(&words[2]),
                old: unquote(&words[5]),
                new: unquote(&words[7]),
            })
        }

        _ => None,
    }
}

/// A statement rewritten for the parser, and where every part of it stood in
/// the text the client sent.
///
/// The two halves are not interchangeable. Spans come from the parser and so
/// point into `sql`; a name, an error position or anything else a client sees
/// has to be quoted from `written`, because that is the text it actually sent.
/// `as_written` is the only bridge, and it exists because no pass here is
/// length-preserving — `""` collapses to one byte and a backtick doubles into
/// two, so equal lengths are a coincidence and not a licence to slice the
/// original at the offsets the rewrite reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalised {
    /// The text to parse.
    pub sql: String,
    /// The text the client sent.
    written: String,
    /// `sql` back to `written`.
    map: Mapping,
}

impl Normalised {
    /// A statement no pass has touched.
    fn untouched(sql: &str) -> Self {
        Self {
            sql: sql.to_owned(),
            written: sql.to_owned(),
            map: Mapping::identity(sql.len()),
        }
    }

    /// What the client wrote where the rewrite says `range`.
    ///
    /// The smallest stretch of the original that covers the whole of `range`,
    /// which for a rewrite is the only answer that cannot be wrong. A name is
    /// a range rather than a point, so a region with no counterpart on one
    /// side — a `""` that collapsed, a space IGNORE_SPACE removed — has to be
    /// inside the answer whenever text on both sides of it is, or the answer
    /// would join two stretches that were never adjacent. Trivia at the very
    /// edges is left out for the same reason it would be wrong to keep it:
    /// nothing in `range` came from it.
    ///
    /// `None` for a range outside the rewritten text.
    pub fn as_written(&self, range: std::ops::Range<usize>) -> Option<&str> {
        if range.start > range.end || range.end > self.sql.len() {
            return None;
        }
        self.written.get(self.map.source(range))
    }

    /// The text the client wrote before `at` in the rewrite begins.
    ///
    /// Deliberately not `as_written(0..at)`, which ends where the range *before*
    /// `at` ends — the far side of anything a pass removed there. This ends
    /// where `at` itself begins, so what it reports is the character the client
    /// actually put in front of that position.
    ///
    /// Which is a different character from the one the parser saw whenever a
    /// pass closed a gap: under IGNORE_SPACE `SELECT (COUNT (*))` reaches the
    /// parser as `SELECT(COUNT(*))`, where the `(` follows the `T` of `SELECT`
    /// and never did in what was sent.
    ///
    /// `None` for a position outside the rewritten text.
    pub fn written_before(&self, at: usize) -> Option<&str> {
        if at > self.sql.len() {
            return None;
        }
        self.written.get(..self.map.start_of(at))
    }

    /// Applies one pass, keeping the record of where the text came from.
    fn rewrite(self, pass: impl FnOnce(&mut Rewriter<'_>)) -> Self {
        let (sql, map) = {
            let mut writer = Rewriter::new(&self);
            pass(&mut writer);
            writer.finish()
        };
        Self {
            sql,
            written: self.written,
            map,
        }
    }
}

/// Rewrites text the parser would reject but that needs no special handling.
pub fn normalise(sql: &str, mode: SqlMode) -> Normalised {
    let mut out = Normalised::untouched(sql);

    // `SET @x := 1` is the same statement as `SET @x = 1`; only the spelling
    // is unsupported, so it is rewritten rather than intercepted.
    if starts_with_keyword(&out.sql, "SET") {
        out = out.rewrite(|w| replace_outside_literals(w, ":=", "="));
    }

    // `DELETE a.*, b.* FROM ...` is the canonical multi-table spelling, and
    // the parser rejects the star. The `.*` carries no meaning MySQL does not
    // already take from the table name, so it is dropped.
    if starts_with_keyword(&out.sql, "DELETE") {
        out = out.rewrite(strip_delete_target_stars);
    }

    // MySQL accepts an IN list whose elements are scalar subqueries; the
    // parser reads the first as `IN (subquery)` and then fails on the comma.
    out = out.rewrite(|w| {
        let all = w.from.len();
        expand_in_subquery_lists(w, 0, all);
    });

    // Under ANSI_QUOTES a double-quoted word is an identifier. Rewriting it to
    // a backtick gives the parser the same meaning without a custom dialect,
    // which sqlparser cannot honour anyway: its MySQL-specific parsing is
    // gated on the concrete dialect type, so a trait wrapper would silently
    // lose AUTO_INCREMENT and much else.
    if mode.ansi_quotes {
        out = out.rewrite(double_quotes_to_backticks);
    }

    // Under IGNORE_SPACE a function name may be separated from its `(`. The
    // parser then reads `LEFT (` as the start of a LEFT JOIN, so the space is
    // closed up. Doing this only when the mode is set keeps the default
    // behaviour untouched.
    if mode.ignore_space {
        out = out.rewrite(close_call_spaces);
    }

    // `EXTRACT(DAY_SECOND FROM x)` names a unit the parser has no keyword for,
    // so the whole call is turned into one the evaluator can reach. Gated on
    // the word rather than inside the pass, because a pass copies the whole
    // statement whether it edits anything or not and almost none of them
    // mention `EXTRACT` at all.
    if contains_the_word(&out.sql, b"extract") {
        out = out.rewrite(compound_extract_to_call);
    }

    // `n MOD 3` is the same operator as `n % 3`, and only the second spelling
    // parses. Last of the passes, so the tokenizer is shown the text the
    // parser will see and the offsets it reports are offsets into the answer.
    mod_operator_to_percent(out)
}

/// A stretch of rewritten text, and where it stood in the text the client sent.
///
/// The two lengths are independent — `src_len` is zero for text a pass
/// inserted out of nothing — so neither side is a function of the other.
/// `verbatim` says the stretch is the same bytes on both sides and an offset
/// inside one is the same offset inside the other; without it a `"x"` rewritten
/// to `` `x` `` would look interchangeable character for character, which is
/// exactly the assumption this type exists to refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    out: usize,
    len: usize,
    src: usize,
    src_len: usize,
    verbatim: bool,
}

/// Where every part of a rewritten statement came from.
///
/// The pieces cover the rewritten text end to end and are ordered by `out`, so
/// every position in it falls in exactly one. The *original* is not covered:
/// text a pass removed is in no piece at all, which is why an answer is a hull
/// and not a union — a range spanning a removal picks that text up because the
/// hull is contiguous, and a range stopping at it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mapping {
    pieces: Vec<Piece>,
    src_len: usize,
}

impl Mapping {
    fn identity(len: usize) -> Self {
        Self {
            pieces: vec![Piece {
                out: 0,
                len,
                src: 0,
                src_len: len,
                verbatim: true,
            }],
            src_len: len,
        }
    }

    /// The smallest range of the original covering `range` of the rewrite.
    fn source(&self, range: std::ops::Range<usize>) -> std::ops::Range<usize> {
        let start = self.start_of(range.start);
        // Never inverted: an empty range in the rewrite maps to an empty one
        // here, wherever the two ends land.
        let end = self.end_of(range.end).max(start);
        start..end
    }

    /// Where a rewritten position starts in the original.
    ///
    /// The first piece the position is *inside*, so a piece that ends exactly
    /// here is passed over — text removed just before the position was not
    /// part of what follows it, and including it would widen the answer for
    /// nothing.
    fn start_of(&self, at: usize) -> usize {
        self.pieces
            .iter()
            .find(|piece| piece.out + piece.len > at)
            .map_or(self.src_len, |piece| {
                if piece.verbatim {
                    piece.src + (at - piece.out)
                } else {
                    // Inside a rewrite, so the whole of what it replaced is
                    // what this position came from.
                    piece.src
                }
            })
    }

    /// Where a rewritten position ends in the original, by the mirror rule.
    fn end_of(&self, at: usize) -> usize {
        self.pieces
            .iter()
            .rev()
            .find(|piece| piece.out < at)
            .map_or(0, |piece| {
                if piece.verbatim {
                    piece.src + (at - piece.out).min(piece.len)
                } else {
                    piece.src + piece.src_len
                }
            })
    }
}

/// Builds one pass's output, and the record of where it came from.
///
/// A pass says what it copies and what it replaces, in order, and the mapping
/// falls out. Composing is what makes that worth doing: a pass reports
/// positions in *its own* input, which is the previous pass's output, and the
/// answer has to be a position in the text the client sent however many passes
/// stand between.
struct Rewriter<'a> {
    /// The text this pass reads.
    from: &'a str,
    /// Where `from` itself came from.
    was: &'a Mapping,
    out: String,
    pieces: Vec<Piece>,
    /// How far through `from` has been emitted.
    copied: usize,
    /// The first piece of `was` not yet carried over. Copying runs forwards,
    /// so the search never has to start again from the beginning.
    next: usize,
}

impl<'a> Rewriter<'a> {
    fn new(text: &'a Normalised) -> Self {
        Self {
            from: &text.sql,
            was: &text.map,
            out: String::with_capacity(text.sql.len()),
            pieces: Vec::new(),
            copied: 0,
            next: 0,
        }
    }

    /// Copies `from` up to `to` unchanged.
    fn copy_to(&mut self, to: usize) {
        let to = to.clamp(self.copied, self.from.len());
        if to == self.copied {
            return;
        }
        let at = self.out.len();
        while let Some(piece) = self.was.pieces.get(self.next).copied() {
            if piece.out >= to {
                break;
            }
            let lo = piece.out.max(self.copied);
            let hi = (piece.out + piece.len).min(to);
            if lo < hi {
                self.pieces.push(if piece.verbatim {
                    Piece {
                        out: at + (lo - self.copied),
                        len: hi - lo,
                        src: piece.src + (lo - piece.out),
                        src_len: hi - lo,
                        verbatim: true,
                    }
                } else {
                    // Only part of an earlier rewrite is being copied, and a
                    // rewrite has no interior positions to divide, so the part
                    // carries the whole of what it replaced.
                    Piece {
                        out: at + (lo - self.copied),
                        len: hi - lo,
                        ..piece
                    }
                });
            }
            if piece.out + piece.len > to {
                break;
            }
            self.next += 1;
        }
        self.out.push_str(&self.from[self.copied..to]);
        self.copied = to;
    }

    /// Emits `with` where `from[at..end]` stood.
    ///
    /// A removal records nothing, because there is nothing in the rewrite for
    /// a piece to cover and the answer does not need one: a range is mapped to
    /// the *hull* of what it covers, so text dropped between two stretches is
    /// inside the answer whenever both of those stretches are, and text
    /// dropped at either edge is outside it. A piece of zero length could
    /// never be found by either end of the lookup.
    fn substitute(&mut self, at: usize, end: usize, with: &str) {
        // A pass has to report its edits in order, and one that does not would
        // otherwise be absorbed rather than caught: `copy_to` clamps backwards
        // and the cursor below takes the later end, so the overlapped input
        // would be dropped and the pieces left out of order — a wrong column
        // name with nothing to show for it. Every pass here walks segments or
        // tokens ascending; this is for the next one that does not.
        debug_assert!(
            at >= self.copied,
            "a rewriting pass reported an edit at {at} after emitting {}",
            self.copied
        );
        self.copy_to(at);
        if !with.is_empty() {
            let source = self.was.source(at..end);
            self.pieces.push(Piece {
                out: self.out.len(),
                len: with.len(),
                src: source.start,
                src_len: source.end - source.start,
                verbatim: false,
            });
        }
        self.out.push_str(with);
        // A piece of `was` this substitution ate is left for the next copy to
        // step over: it contributes nothing once `copied` is past it, and the
        // cursor advances then rather than here.
        self.copied = end.max(self.copied);
    }

    fn finish(mut self) -> (String, Mapping) {
        let all = self.from.len();
        self.copy_to(all);
        (
            self.out,
            Mapping {
                pieces: self.pieces,
                src_len: self.was.src_len,
            },
        )
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
/// The word each `%` was written as is not recorded anywhere: the mapping is
/// what carries it, and a column named after the text it was written as reads
/// `MOD` — or `mod`, which MySQL names just as faithfully — back out of the
/// original through that.
fn mod_operator_to_percent(text: Normalised) -> Normalised {
    // Nearly every statement is tokenized here for nothing, so the cheap test
    // comes first.
    if !contains_the_word(&text.sql, b"mod") {
        return text;
    }

    let Ok(tokens) = Tokenizer::new(&MySqlDialect {}, &text.sql).tokenize_with_location() else {
        // Text the tokenizer refuses is text the parser will refuse, with a
        // better message than a half-applied rewrite could produce.
        return text;
    };

    // Whitespace and comments are not operands, and MySQL allows any amount of
    // either around an operator, so the neighbour tests read past them.
    let code: Vec<&TokenWithSpan> = tokens
        .iter()
        .filter(|t| !matches!(t.token, Token::Whitespace(_)))
        .collect();

    let mut substituted = false;
    let (out, map) = {
        let mut writer = Rewriter::new(&text);
        let sql = writer.from;

        for (index, token) in code.iter().enumerate() {
            // A backquoted `mod` is `Keyword::NoKeyword`, so a column of that
            // name never reaches the neighbour tests.
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
            writer.substitute(start, end, "%");
            substituted = true;
        }

        writer.finish()
    };

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
    // `contains_the_word` rejects the rest on a byte scan, and nothing
    // substituted means nothing to check.
    if !substituted || Parser::parse_sql(&MySqlDialect {}, &out).is_err() {
        return text;
    }

    Normalised {
        sql: out,
        written: text.written,
        map,
    }
}

/// Whether `word` appears as a whole word, ignoring case.
///
/// A whole word and not a substring, because the passes that ask are the
/// expensive ones and the near misses are common: `model`, `mode` and
/// `modified_at` are ordinary column names in the target schema, and
/// `JSON_EXTRACT` appears far more often than `EXTRACT` does. A substring test
/// would hand every statement mentioning one a tokenizer pass it can never
/// use.
fn contains_the_word(sql: &str, word: &[u8]) -> bool {
    let bytes = sql.as_bytes();
    bytes.windows(word.len()).enumerate().any(|(at, window)| {
        window.eq_ignore_ascii_case(word)
            && at
                .checked_sub(1)
                .is_none_or(|before| !is_ident_byte(bytes[before]))
            && bytes
                .get(at + word.len())
                .is_none_or(|after| !is_ident_byte(*after))
    })
}

/// Rewrites `EXTRACT(DAY_SECOND FROM x)` to a call the evaluator can reach.
///
/// MySQL takes twenty units after `EXTRACT`; sqlparser 0.62 has a keyword for
/// nine of them, and `MySqlDialect::allow_extract_custom()` is false, so the
/// other eleven — `crate::temporal::Compound` — fail at parse time with a 1064
/// naming the unit. `crate::dialect` says why a wrapper `Dialect` cannot fix
/// that: sqlparser gates its MySQL parsing on the concrete type at 94 sites.
///
/// So the call becomes `ultralight_extract('DAY_SECOND', x)`, which parses as
/// an ordinary function and is answered in `crate::expr`. Only the head of the
/// call is replaced — `EXTRACT` through `FROM`, four tokens — so the argument
/// is carried over untouched however long it is and `EXTRACT`'s own closing
/// parenthesis becomes the call's. Nothing has to balance anything.
///
/// The pattern cannot be anything else, so unlike the `MOD` pass this one does
/// not parse its answer before keeping it. `EXTRACT` is reserved in MySQL and
/// cannot be a bare name; the only thing `EXTRACT ( <word> FROM` can be is
/// this function; and a *column* named `day_second` in that position reads as
/// the unit on 8.3 too, so the rewrite does not change what it means.
///
/// The name it uses is spelled the way `ultralight_snapshot` is, and carries
/// the same trade: MySQL answers 1305 for a call to it and this engine
/// answers, so the accepted grammar is a shade wider than MySQL's. A rewrite
/// is text in and text out, so anything it can emit a client can also type —
/// what that buys is that the extra spelling is one MySQL *rejects* rather
/// than one it reads differently.
fn compound_extract_to_call(w: &mut Rewriter<'_>) {
    let sql = w.from;
    let Ok(tokens) = Tokenizer::new(&MySqlDialect {}, sql).tokenize_with_location() else {
        // Text the tokenizer refuses is text the parser will refuse, with a
        // better message than a half-applied rewrite could produce.
        return;
    };
    // Whitespace and comments are not part of the shape, and MySQL allows any
    // amount of either between these four tokens.
    let code: Vec<&TokenWithSpan> = tokens
        .iter()
        .filter(|t| !matches!(t.token, Token::Whitespace(_)))
        .collect();

    for (index, token) in code.iter().enumerate() {
        // A backquoted `extract` is a name rather than the construct, so it
        // never starts a match.
        if !matches!(&token.token, Token::Word(word)
            if word.keyword == Keyword::EXTRACT && word.quote_style.is_none())
        {
            continue;
        }
        let (Some(open), Some(unit), Some(from)) = (
            code.get(index + 1),
            code.get(index + 2),
            code.get(index + 3),
        ) else {
            continue;
        };
        if !matches!(open.token, Token::LParen) {
            continue;
        }
        let Token::Word(unit) = &unit.token else {
            continue;
        };
        // Backquoted, it is a name and not a unit — and MySQL agrees:
        // ``EXTRACT(`DAY_SECOND` FROM x)`` is a 1064 there.
        if unit.quote_style.is_some() {
            continue;
        }
        let Some(compound) = Compound::parse(&unit.value) else {
            continue;
        };
        if !matches!(&from.token, Token::Word(word) if word.keyword == Keyword::FROM) {
            continue;
        }
        let (Some(start), Some(end)) = (
            byte_offset(sql, token.span.start),
            byte_offset(sql, from.span.end),
        ) else {
            continue;
        };
        // The unit goes in as a string rather than as the bare word MySQL
        // wrote, because a string is a *value*: every walk over the AST that
        // looks for column references steps over it, where a bare keyword
        // parses as an identifier indistinguishable from a column and needs a
        // special case in each of them — which is what `value_arguments` is
        // for `TIMESTAMPDIFF(DAY, a, b)`.
        w.substitute(start, end, &compound_extract_call(compound));
    }
}

/// The head of the call a compound `EXTRACT` becomes, up to its argument.
fn compound_extract_call(compound: Compound) -> String {
    format!("ultralight_extract('{}', ", compound.name())
}

/// Puts `EXTRACT(DAY_SECOND FROM x)` back into text rendered from the parsed
/// tree.
///
/// A column's *name* reads back through `Normalised`, which is exact. Anything
/// rendered from the tree instead has no mapping to read — an
/// `ONLY_FULL_GROUP_BY` message names the select item it walked, and a view
/// keeps what its query renders to — and would show a client the internal
/// spelling of a function MySQL answers 1305 for.
///
/// Text in and text out, like the pass it undoes, and it is the same
/// substitution backwards: the head of the call up to the argument, with the
/// closing parenthesis left where it is. A string literal spelling the head
/// out character for character would be rewritten too, which is the same trade
/// the forward pass makes and no worse than it.
pub fn extract_as_written(rendered: &str) -> String {
    // The cheap test first: nothing else here is reached by a statement that
    // never went through the pass, which is nearly all of them.
    if !rendered.contains("ultralight_extract(") {
        return rendered.to_owned();
    }
    let mut out = rendered.to_owned();
    for compound in Compound::ALL {
        let call = compound_extract_call(compound);
        if out.contains(&call) {
            out = out.replace(&call, &format!("EXTRACT({} FROM ", compound.name()));
        }
    }
    out
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
fn strip_delete_target_stars(writer: &mut Rewriter<'_>) {
    let sql = writer.from;
    let bytes = sql.as_bytes();

    for (kind, start, end) in segments(sql) {
        if kind != Segment::Code {
            continue;
        }

        let mut i = start;
        while i < end {
            if bytes[i] == b'.' && bytes.get(i + 1) == Some(&b'*') {
                writer.substitute(i, i + 2, "");
                i += 2;
                continue;
            }
            // Everything from the target list's `FROM` onward is left alone.
            if word_at(bytes, i, b"FROM") {
                return;
            }
            i += utf8_len(bytes[i]);
        }
    }
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

/// The parts of a qualified identifier, each with its quoting undone.
///
/// Two things trimming the quotes off cannot do, and both were wrong here.
/// A quote inside a quoted identifier is *doubled* rather than escaped, so
/// ``ALTER TABLE `we``ird` RENAME INDEX`` addressed a table ``we``ird`` no
/// catalog holds; and only a dot *outside* the quotes separates one part from
/// the next, so splitting the text on `.` first made ``ALTER TABLE `my.table` ``
/// a table `table` in a database `my`.
///
/// The parts come back as a list rather than joined for that second reason:
/// rejoining them with a dot is the step nothing downstream can undo.
fn unquote_parts(word: &str) -> Vec<String> {
    let bytes = word.as_bytes();
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut i = 0;

    while i < bytes.len() {
        // `classify` is the one place that knows where a quoted run ends,
        // including that a doubled quote does not end it.
        if let Some((Segment::Quoted, end)) = classify(bytes, i) {
            current.push_str(&quoted_value(&word[i..end]));
            i = end;
            continue;
        }
        if bytes[i] == b'.' {
            parts.push(std::mem::take(&mut current));
            i += 1;
            continue;
        }
        // A char at a time rather than a byte, so a non-ASCII identifier
        // survives: `tabké` is a name MySQL accepts.
        let ch = word[i..].chars().next().expect("i is a char boundary");
        current.push(ch);
        i += ch.len_utf8();
    }

    parts.push(current);
    parts
}

/// A single identifier, for the names that carry no qualifier.
fn unquote(word: &str) -> String {
    unquote_parts(word).join(".")
}

/// The value of one quoted run, `text` beginning at its opening quote.
///
/// A doubled quote reads as one, which is the escaping MySQL's own renderers
/// apply. A backslash escape is left as written: undoing those is
/// `NO_BACKSLASH_ESCAPES` territory, and that mode is not modelled here at all.
fn quoted_value(text: &str) -> String {
    let quote = &text[..1];
    let inner = text
        .strip_prefix(quote)
        // An unterminated run has no closing quote to strip, and everything
        // after the opening one is its value.
        .and_then(|t| t.strip_suffix(quote))
        .unwrap_or(&text[1..]);
    inner.replace(&format!("{quote}{quote}"), quote)
}

/// The text of a `PREPARE ... FROM '<sql>'` literal, as written.
///
/// Its own entry point rather than `unquote`, because a statement is not an
/// identifier: a dot in it separates nothing. Trimming the quotes off the
/// ends — which is what this was — turned a doubled quote at the end of the
/// statement into part of the closing run, so `PREPARE s FROM 'SELECT ''x'''`
/// prepared the unparseable `SELECT ''x`.
pub(crate) fn unquote_literal(text: &str) -> String {
    match classify(text.as_bytes(), 0) {
        // The whole of it, or it is not a literal: `@v` is a variable and
        // `'a' 'b'` is two runs, neither of which this may take apart.
        Some((Segment::Quoted, end)) if end == text.len() => quoted_value(text),
        _ => text.to_owned(),
    }
}

/// Replaces every occurrence in code, leaving literals and comments alone.
fn replace_outside_literals(writer: &mut Rewriter<'_>, from: &str, to: &str) {
    let sql = writer.from;
    for (kind, start, end) in segments(sql) {
        if kind != Segment::Code {
            continue;
        }
        let mut at = start;
        while let Some(found) = sql[at..end].find(from) {
            let found = at + found;
            writer.substitute(found, found + from.len(), to);
            at = found + from.len();
        }
    }
}

/// Rewrites `"identifier"` as `` `identifier` ``.
///
/// Only quoted runs the scanner classified as such are converted, so a stray
/// double quote inside a comment cannot open an identifier that swallows the
/// rest of the statement.
///
/// The rewrite is not length-preserving in either direction — `""` collapses
/// to one byte and a backtick doubles into two — so each quoted run is one
/// piece of the mapping rather than a stretch of it that happens to line up.
fn double_quotes_to_backticks(writer: &mut Rewriter<'_>) {
    let sql = writer.from;
    for (kind, start, end) in segments(sql) {
        let text = &sql[start..end];
        if kind != Segment::Quoted || !text.starts_with('"') {
            continue;
        }
        let inner = text
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .unwrap_or(&text[1..]);
        let quoted = format!("`{}`", inner.replace("\"\"", "\"").replace('`', "``"));
        writer.substitute(start, end, &quoted);
    }
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
///
/// Written over a range of the statement rather than over a string of its own
/// so that a nested list recurses in place: a branch rewritten separately and
/// spliced back would have to have its own mapping composed into this one, and
/// the offsets it reported would be offsets into a string no caller has.
fn expand_in_subquery_lists(writer: &mut Rewriter<'_>, from: usize, to: usize) {
    let sql = writer.from;
    let bytes = sql.as_bytes();
    let mut i = from;

    while i < to {
        // Literals and comments are code to nobody; `IN` inside one is text.
        if let Some((_, end)) = classify(bytes, i) {
            i = end.max(i + 1).min(to);
            continue;
        }

        let is_in_keyword = to - i >= 2
            && bytes[i..i + 2].eq_ignore_ascii_case(b"IN")
            && i.checked_sub(1).is_none_or(|p| !is_ident_byte(bytes[p]))
            && bytes.get(i + 2).is_none_or(|b| !is_ident_byte(*b));

        if !is_in_keyword {
            i += utf8_len(bytes[i]);
            continue;
        }

        // Whitespace *and comments* may sit between the keyword and the list.
        let open = skip_trivia(bytes, i + 2);
        let Some(close) = (bytes.get(open) == Some(&b'('))
            .then(|| matching_paren(bytes, open))
            .flatten()
            // A list closing outside the range being walked is one belonging
            // to an enclosing call, and rewriting it here would emit text out
            // of order.
            .filter(|close| *close < to)
        else {
            i += 2;
            continue;
        };

        let inner = open + 1;
        let parts = split_top_level_commas(&sql[inner..close]);
        if parts.len() < 2
            || !parts
                .iter()
                .all(|p| is_parenthesised_select(&sql[inner + p.start..inner + p.end]))
        {
            i += 2;
            continue;
        }

        // Each branch keeps its parentheses: one may end in LIMIT, and
        // `SELECT ... LIMIT 1 UNION ALL SELECT ...` binds the LIMIT to the
        // whole union and stops parsing. Branches are expanded in turn so a
        // nested list is not left behind.
        let mut previous = None;
        for part in &parts {
            let branch = trimmed(sql, inner + part.start..inner + part.end);
            match previous {
                // The comma and the space either side of it together, because
                // the three are one edit: what stands there afterwards came
                // from all of them and from none of them in particular.
                Some(end) => writer.substitute(end, branch.start, " UNION ALL "),
                None => writer.substitute(inner, branch.start, ""),
            }
            expand_in_subquery_lists(writer, branch.start, branch.end);
            previous = Some(branch.end);
        }
        if let Some(end) = previous {
            writer.substitute(end, close, "");
        }
        i = close + 1;
    }
}

/// `range` with the whitespace at either end left out.
fn trimmed(sql: &str, range: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let text = &sql[range.clone()];
    let start = range.start + (text.len() - text.trim_start().len());
    start..(start + text.trim().len())
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
/// By range rather than by slice, because the caller needs to say where each
/// part stood as well as what it says.
fn split_top_level_commas(inner: &str) -> Vec<std::ops::Range<usize>> {
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
                parts.push(start..i);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(start..inner.len());
    parts
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Removes whitespace between an identifier and a following `(`, in code only.
///
/// Doing this blind to comments deleted the newline that terminates a `--`
/// comment, commenting out everything after it.
fn close_call_spaces(writer: &mut Rewriter<'_>) {
    let sql = writer.from;
    let parts = segments(sql);

    for (index, (kind, start, end)) in parts.iter().enumerate() {
        if *kind != Segment::Code {
            continue;
        }
        let text = &sql[*start..*end];

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
            // The character before the run, which is the last one that will
            // have been emitted: a run is only ever removed with a `(` after
            // it, so no removal can change what precedes the next one.
            let preceded = preceded
                && sql[..*start + i]
                    .chars()
                    .next_back()
                    .is_some_and(|p| p.is_alphanumeric() || p == '_');

            if preceded && follows_paren {
                writer.substitute(*start + i, *start + j, "");
            }
            i = j;
        }
    }
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

    /// The spellings of the four diagnostics statements, read out of the text
    /// because sqlparser has a node for none of them. #0204.
    #[test]
    fn recognises_the_diagnostics_statements() {
        let show = |sql| match intercept(sql) {
            Some(Intercepted::ShowDiagnostics(show)) => show,
            other => panic!("{sql} was read as {other:?}"),
        };

        assert_eq!(
            show("SHOW WARNINGS"),
            ShowDiagnostics {
                errors_only: false,
                count_only: false,
                limit: None
            }
        );
        assert!(show("show errors").errors_only);
        assert_eq!(show("SHOW WARNINGS LIMIT 2").limit, Some((0, 2)));
        // `words_of` splits on a comma, so the offset form arrives with or
        // without the space after it.
        assert_eq!(show("SHOW WARNINGS LIMIT 1,2").limit, Some((1, 2)));
        assert_eq!(show("SHOW WARNINGS LIMIT 1, 2").limit, Some((1, 2)));

        // The count forms, whose middle is not one word on every spelling: the
        // parentheses do not separate words, so `COUNT( * )` is three.
        assert!(show("SHOW COUNT(*) WARNINGS").count_only);
        assert!(show("SHOW COUNT( * ) WARNINGS").count_only);
        assert!(show("SHOW COUNT(*) ERRORS").errors_only);

        // And what this does *not* read, which matters as much: an unreadable
        // form has to reach the parser and be refused there, rather than
        // arriving here as the nearest thing that does parse.
        for sql in [
            "SHOW WARNINGS LIMIT x",
            "SHOW WARNINGS LIMIT 1, 2, 3",
            "SHOW COUNT(*) NOTES",
            "SHOW TABLES",
            "SHOW VARIABLES",
        ] {
            assert!(
                !matches!(intercept(sql), Some(Intercepted::ShowDiagnostics(_))),
                "{sql} must not be read as a diagnostics statement"
            );
        }
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
                table: vec!["T".into()],
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
            assert!(!contains_the_word(sql, b"mod"), "{sql}");
        }
        for sql in ["SELECT n MOD 3", "SELECT mod FROM t", "mod", "SELECT a-mod"] {
            assert!(contains_the_word(sql, b"mod"), "{sql}");
        }
    }

    /// The same gate in front of the compound-`EXTRACT` pass, where the near
    /// miss is `JSON_EXTRACT` — which the target schema uses far more often
    /// than it uses `EXTRACT`.
    #[test]
    fn the_fast_path_matches_extract_only_as_a_word() {
        for sql in [
            "SELECT JSON_EXTRACT(doc, '$.a') FROM t",
            "SELECT extracted, extract_id FROM t",
        ] {
            assert!(!contains_the_word(sql, b"extract"), "{sql}");
        }
        for sql in [
            "SELECT EXTRACT(DAY_SECOND FROM d) FROM t",
            "select extract(year from d)",
        ] {
            assert!(contains_the_word(sql, b"extract"), "{sql}");
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
        assert_eq!(unquote_parts(r#""db"."t""#), vec!["db", "t"]);
        assert_eq!(unquote_parts("`db`.`t`"), vec!["db", "t"]);
        assert_eq!(unquote_parts("plain"), vec!["plain"]);
    }

    /// The two things trimming cannot do, on the statement that reaches them.
    #[test]
    fn a_quoted_name_keeps_its_dots_and_undoubles_its_quotes() {
        // One part, not two: the dot is inside the name.
        assert_eq!(unquote_parts("`my.table`"), vec!["my.table"]);
        assert_eq!(
            unquote_parts("`my.db`.`my.table`"),
            vec!["my.db", "my.table"]
        );
        // One backtick, not the two the name was written with.
        assert_eq!(unquote_parts("`we``ird`"), vec!["we`ird"]);
        assert_eq!(unquote_parts(r#""we""ird""#), vec![r#"we"ird"#]);
        assert_eq!(unquote_parts("`tabké`"), vec!["tabké"]);
    }

    #[test]
    fn the_rename_index_table_keeps_its_dot() {
        assert_eq!(
            intercept("ALTER TABLE `my.table` RENAME INDEX a TO b"),
            Some(Intercepted::RenameIndex {
                table: vec!["my.table".into()],
                old: "a".into(),
                new: "b".into(),
            })
        );
    }

    /// A doubled quote inside a prepared statement's source is one quote.
    #[test]
    fn a_prepared_source_undoubles_its_quotes() {
        let doubled = "'SELECT ''x'' AS c'";
        assert_eq!(unquote_literal(doubled), "SELECT 'x' AS c");
        // A variable is not a literal and must reach the caller untouched,
        // because that is how the caller tells the two apart.
        assert_eq!(unquote_literal("@sql"), "@sql");
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

#[cfg(test)]
mod offset_mapping {
    use super::*;

    /// MySQL's `ANSI` as 8.3 expands it, so a fixture cannot describe one mode
    /// while its name describes another.
    const ANSI: SqlMode = SqlMode {
        ansi_quotes: true,
        pipes_as_concat: true,
        ignore_space: true,
        real_as_float: true,
        strict: false,
        only_full_group_by: true,
        no_unsigned_subtraction: false,
    };

    fn quotes() -> SqlMode {
        SqlMode {
            ansi_quotes: true,
            ..SqlMode::default()
        }
    }

    fn space() -> SqlMode {
        SqlMode {
            ignore_space: true,
            ..SqlMode::default()
        }
    }

    /// What the client wrote where the rewrite says `needle`.
    ///
    /// By searching the *rewritten* text for the range, which is how the
    /// executor reaches this: a span points into what the parser was given.
    fn written(sql: &str, mode: SqlMode, needle: &str) -> String {
        let normalised = normalise(sql, mode);
        let at = normalised
            .sql
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not in {:?}", normalised.sql));
        normalised
            .as_written(at..at + needle.len())
            .unwrap_or_else(|| panic!("{needle:?} maps outside {sql:?}"))
            .to_owned()
    }

    /// The whole point: an offset in the rewrite is not an offset in the
    /// original, even where the two texts are the same length.
    #[test]
    fn a_rewritten_range_maps_back_to_the_text_it_came_from() {
        // Same length, and positional — the case the obvious repair would
        // have got right by luck.
        assert_eq!(
            written(r#"SELECT "x" + 1 FROM P"#, quotes(), "`x` + 1"),
            r#""x" + 1"#
        );
        // One byte shorter: `""` collapses to one quote.
        assert_eq!(
            written(r#"SELECT "a""b" + 1 FROM P"#, quotes(), r#"`a"b` + 1"#),
            r#""a""b" + 1"#
        );
        // One byte longer: a backtick doubles.
        assert_eq!(
            written("SELECT \"a`b\" + 1 FROM P", quotes(), "`a``b` + 1"),
            "\"a`b\" + 1"
        );
        // Removed text, which has no counterpart at all: the space is inside
        // the answer because text on both sides of it is.
        assert_eq!(
            written("SELECT COUNT (*) FROM P", space(), "COUNT(*)"),
            "COUNT (*)"
        );
        // A range that is exactly one rewrite and nothing else, so both ends
        // of it land inside the same piece.
        assert_eq!(written(r#"SELECT "x" FROM P"#, quotes(), "`x`"), r#""x""#);
        assert_eq!(
            written("SELECT n MOD 3", SqlMode::default(), "n % 3"),
            "n MOD 3"
        );
        assert_eq!(
            written("SELECT n mod 3", SqlMode::default(), "n % 3"),
            "n mod 3"
        );
    }

    /// Every pass composed, in the order `normalise` runs them.
    #[test]
    fn the_passes_compose() {
        assert_eq!(
            written(
                "SELECT LENGTH (\"a\"\"b\") MOD 2 FROM P",
                ANSI,
                "LENGTH(`a\"b`) % 2"
            ),
            "LENGTH (\"a\"\"b\") MOD 2"
        );
        // Two rewrites of different signs in one range: the identifier grows
        // by a byte where the space before the call loses one.
        assert_eq!(
            written("SELECT ABS (\"a`b\") FROM P", ANSI, "ABS(`a``b`)"),
            "ABS (\"a`b\")"
        );
    }

    /// A range that begins at a rewrite an *earlier* pass made, which is the
    /// case composing exists for: the `MOD` pass copies the identifier the
    /// ANSI_QUOTES pass produced, and the answer is still the original.
    #[test]
    fn a_range_beginning_at_a_carried_rewrite_maps_back() {
        assert_eq!(
            written(r#"SELECT "x" MOD 3 FROM P"#, quotes(), "`x` % 3"),
            r#""x" MOD 3"#
        );
    }

    /// A range that stops in the middle of a rewrite covers the whole of it.
    ///
    /// A rewrite has no interior position that means anything in the original,
    /// so half of one is still all of what it replaced. Asserted here and not
    /// through a column name, because a name's span lands on token boundaries
    /// and never inside one — this is the level the rule exists at.
    #[test]
    fn a_range_ending_inside_a_rewrite_covers_all_of_it() {
        let sql = "SELECT \"a`b\" + 1";
        let normalised = normalise(sql, quotes());
        assert_eq!(normalised.sql, "SELECT `a``b` + 1");
        // Two bytes into the rewritten identifier, which is one byte into the
        // five the client wrote — a place the original does not have.
        assert_eq!(normalised.as_written(0..9), Some("SELECT \"a`b\""));
    }

    /// A range that stops at a rewrite rather than covering it, which is where
    /// a mapping can be too wide as easily as too narrow.
    #[test]
    fn trivia_at_the_edge_of_a_range_is_left_out() {
        // The space IGNORE_SPACE removed sits at the end of this range, so it
        // is outside what the range covers and outside the answer.
        assert_eq!(
            written("SELECT COUNT (*) FROM P", space(), "COUNT"),
            "COUNT"
        );
        assert_eq!(written("SELECT COUNT (*) FROM P", space(), "(*)"), "(*)");
    }

    /// A pass that reports its edits out of order is a bug in that pass, and
    /// the builder says so rather than absorbing it.
    ///
    /// Asserted here because there is no statement that reaches it: every pass
    /// in this module walks segments or tokens ascending. `Rewriter` is the
    /// facility the next pass will be written against, and absorbing a
    /// mis-ordered edit would drop the overlapped input and leave the pieces
    /// unordered — a wrong column name with nothing to show for it.
    #[test]
    #[should_panic(expected = "reported an edit at")]
    fn an_edit_reported_out_of_order_is_refused() {
        let _ = Normalised::untouched("SELECT a + b").rewrite(|writer| {
            writer.substitute(9, 10, "-");
            writer.substitute(7, 8, "z");
        });
    }

    /// A statement no pass touched maps to itself, which is every statement
    /// the default mode sees.
    #[test]
    fn an_untouched_statement_maps_to_itself() {
        let sql = "SELECT a + 1, CONCAT('x', 'y') FROM t";
        let normalised = normalise(sql, SqlMode::default());
        assert_eq!(normalised.sql, sql);
        assert_eq!(normalised.as_written(0..sql.len()), Some(sql));
        assert_eq!(normalised.as_written(7..12), Some("a + 1"));
        // Out of range rather than a panic or a wrong slice.
        assert_eq!(normalised.as_written(0..sql.len() + 1), None);
    }

    /// The IN-list expansion inserts text with no counterpart in the original
    /// and drops text with none in the rewrite, both in the same list.
    #[test]
    fn an_expanded_in_list_maps_back_to_the_list_as_written() {
        let sql = "SELECT a FROM t WHERE b IN ( (SELECT 1) ,  (SELECT 2) )";
        let normalised = normalise(sql, SqlMode::default());
        assert_eq!(
            normalised.sql,
            "SELECT a FROM t WHERE b IN ((SELECT 1) UNION ALL (SELECT 2))"
        );
        let at = normalised.sql.find("((SELECT").unwrap();
        let end = normalised.sql.len();
        assert_eq!(
            normalised.as_written(at..end),
            Some("( (SELECT 1) ,  (SELECT 2) )")
        );
        // A branch on its own, with the inserted separator outside it.
        let branch = normalised.sql.find("(SELECT 2)").unwrap();
        assert_eq!(
            normalised.as_written(branch..branch + "(SELECT 2)".len()),
            Some("(SELECT 2)")
        );
    }

    /// `SET @x := 1`, the one pass whose statement has no columns to name.
    /// Mapped anyway, because a pass that reports nothing is a pass the next
    /// reader has to remember is different.
    #[test]
    fn the_assignment_spelling_maps_back() {
        assert_eq!(
            written("SET @x := 1", SqlMode::default(), "@x = 1"),
            "@x := 1"
        );
    }

    /// And the DELETE star, for the same reason.
    ///
    /// Both edge rules in one statement: the star after `a` is inside the
    /// range and comes back, the star after `b` is at the range's end and is
    /// only reached by a range that covers the character past it.
    #[test]
    fn a_stripped_delete_star_maps_back() {
        let sql = "DELETE a.*, b.* FROM a JOIN b ON a.id = b.id";
        assert_eq!(written(sql, SqlMode::default(), "a, b"), "a.*, b");
        assert_eq!(written(sql, SqlMode::default(), "a, b "), "a.*, b.* ");
    }
}
