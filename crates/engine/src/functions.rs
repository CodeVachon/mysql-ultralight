//! Scalar functions, LIKE matching, and CAST.
//!
//! Aggregates are not here: they need the whole group rather than one row, so
//! the executor handles them.

use sqlparser::ast::DataType;

use crate::error::{unsupported, EngineError, EngineResult};
use crate::json;
use crate::session::Session;
use crate::temporal;
use crate::value::Value;

/// The functions that mean something when handed a NULL.
///
/// Everything else propagates: MySQL returns NULL from a scalar function if any
/// argument is NULL, and the list of exceptions is short enough to write down.
/// Keeping it as a list of *exceptions* rather than a check inside each
/// function is what makes the rule hold for the next function added — the safe
/// answer is the one you get by not thinking about it.
///
/// Three kinds are here. Those whose whole purpose is a NULL test
/// (`COALESCE`, `IFNULL`, `NVL`, `IF`, `ISNULL`, `NULLIF`); `CONCAT_WS`, which
/// documents that it skips NULL arguments where `CONCAT` propagates them; and
/// the JSON constructors, which store a JSON null rather than becoming one —
/// `JSON_ARRAY(NULL)` is `[null]`, not NULL.
const NULL_TOLERANT: &[&str] = &[
    "coalesce",
    "ifnull",
    "nvl",
    "if",
    "isnull",
    "nullif",
    "concat_ws",
    "json_array",
    "json_object",
    "json_array_append",
    "json_set",
];

/// Every function `call` dispatches, in sorted order.
///
/// A second list of the names the match below already spells out, which is a
/// cost worth naming. It exists so the NULL rule can recognise a function
/// before short-circuiting it: without it, `SELECT NOT_A_FUNCTION(NULL)`
/// answers NULL instead of saying the function does not exist, and a query
/// over a column that happens to be all NULL succeeds until the day a row has
/// a value in it.
///
/// The two are held together by `every_known_name_dispatches`. A name in the
/// match and missing here fails as unsupported rather than answering wrongly,
/// which is the direction to drift in.
const KNOWN: &[&str] = &[
    "abs",
    "ceil",
    "ceiling",
    "char_length",
    "character_length",
    "coalesce",
    "concat",
    "concat_ws",
    "connection_id",
    "curdate",
    "current_date",
    "current_time",
    "current_timestamp",
    "current_user",
    "curtime",
    "database",
    "date",
    "date_format",
    "datediff",
    "day",
    "dayofmonth",
    "dayofweek",
    "dayofyear",
    "floor",
    "greatest",
    "hour",
    "if",
    "ifnull",
    "instr",
    "isnull",
    "json_array",
    "json_array_append",
    "json_contains",
    "json_extract",
    "json_keys",
    "json_length",
    "json_merge_patch",
    "json_object",
    "json_quote",
    "json_set",
    "json_type",
    "json_unquote",
    "json_valid",
    "last_day",
    "last_insert_id",
    "lcase",
    "least",
    "left",
    "length",
    "localtime",
    "localtimestamp",
    "locate",
    "lower",
    "lpad",
    "ltrim",
    "md5",
    "microsecond",
    "mid",
    "minute",
    "mod",
    "month",
    "now",
    "nullif",
    "nvl",
    "octet_length",
    "point",
    "position",
    "quarter",
    "regexp_instr",
    "regexp_like",
    "regexp_replace",
    "regexp_substr",
    "replace",
    "reverse",
    "right",
    "round",
    "row_count",
    "rpad",
    "rtrim",
    "schema",
    "second",
    "session_user",
    "soundex",
    "space",
    "st_distance_sphere",
    "substr",
    "substring",
    "sysdate",
    "system_user",
    "trim",
    "truncate",
    "ucase",
    "unix_timestamp",
    "upper",
    "user",
    "version",
    "weekday",
    "year",
];

/// MySQL's SOUNDEX over a character string, which is the classic algorithm
/// with three of its own answers to the places the variants disagree.
///
/// Measured against 8.3 rather than taken from a description of Soundex:
///
/// - A vowel does *not* break a run of one code. `TYMCZAK` is `T520` and not
///   `T522`, because the `K` is still reading as a repeat of the `Z` across
///   the `A` between them. `H`, `W` and `Y` behave the same way.
/// - The first letter seeds that run, so a second letter sharing its code is
///   dropped: `PFISTER` is `P236`, the `F` swallowed by the `P`.
/// - The result is *not* truncated to four. `ASHCRAFT` is `A2613` and
///   `WASHINGTON` is `W25235`. Only a short one is padded.
///
/// A string with no letter in it gives the empty string, not NULL — NULL is
/// only for a NULL argument, which is answered before this is reached.
fn soundex(text: &str) -> String {
    // MySQL's letter test for a multi-byte charset is a range over the
    // *codepoint* and not a Unicode property: ASCII, or U+00C0 and above.
    // `µ` and `ª` are alphabetic to Rust and not to MySQL, and `×` is the
    // reverse. Verified over every codepoint outside ASCII — 63,400 in the
    // BMP and all 1,048,576 above it — against 8.3 on a utf8mb4 connection:
    // the character is emitted whole, byte for byte as it arrived, with no
    // case folding, and it codes as zero.
    //
    // #0074 recorded this as only an approximation, on the strength of
    // `SOUNDEX('€abc')` measuring as the single byte 0xE2 followed by `120`.
    // That measurement was taken through a *latin1* connection, which hands
    // MySQL each UTF-8 byte as its own latin1 character — and the lead byte of
    // a multi-byte sequence is 0xC0 or above, so it passes this very test
    // while the continuation bytes fail it. The bytes come back looking
    // truncated because they are a different string, not because SOUNDEX
    // decoded one badly.
    soundex_of(
        text.chars()
            .filter(|c| c.is_ascii_alphabetic() || (*c as u32) >= 0xC0),
    )
}

/// MySQL's SOUNDEX over a binary string, which is a different letter test.
///
/// A single-byte charset uses its own ctype table rather than the codepoint
/// range [`soundex`] applies, and the binary charset classifies only ASCII:
/// measured on 8.3, of all 256 byte values exactly `A`-`Z` and `a`-`z` are
/// letters, so `SOUNDEX(CAST('éabc' AS BINARY))` is `A120` where the same
/// characters as text are `é120`. Every byte above 0x7F is skipped as though
/// it were punctuation, which is why an all-high-byte binary string gives the
/// empty string where the text reading gives a four-character answer.
///
/// So the result is always ASCII, whatever the input held, and stays a
/// `Value::Text` — the first letter is emitted verbatim and only a letter can
/// be first.
///
/// This is the *only* reading a binary string gets here, which is worth saying
/// because a bitwise operator refuses one for the opposite reason: `X'0F' & 1`
/// and `_binary'ab' & 1` want different answers out of the same `Value`. For
/// SOUNDEX they do not — a hex literal, a `CAST(x AS BINARY)`, a `BINARY 'x'`
/// and a VARBINARY column all measure as `A120` over those bytes. So does a
/// `_binary` literal on MySQL, which this engine cannot evaluate at all: an
/// introducer parses to a prefixed expression and is refused.
///
/// What this does *not* reach is a binary string that has been through another
/// string function first. MySQL keeps the binary charset across `CONCAT`,
/// `SUBSTRING`, `TRIM` and `UPPER`, and this engine does not: over a VARBINARY
/// column holding `éabc`, all four are `A120` on 8.3, while here the first
/// three are `é120` and `UPPER` is `É120` — case-folding the accent as well,
/// because the bytes have become text by then. The charset is a property of
/// the *expression* in MySQL and only of the `Value` here, and every one of
/// those functions builds a `Value::Text`. That is a gap in charset
/// propagation rather than in this function, and it is the reason this
/// dispatch is on the value rather than on a type read off the expression.
///
/// A bound parameter is not a binary string, which is measured rather than
/// assumed: a `Buffer` bound through mysql2 answers `é120` on 8.3, because the
/// placeholder is in a text context and MySQL converts the bytes to the
/// statement's charset. The engine agrees, by decoding a parameter whose bytes
/// are valid UTF-8 as text.
fn soundex_binary(bytes: &[u8]) -> String {
    soundex_of(
        bytes
            .iter()
            .copied()
            .filter(u8::is_ascii_alphabetic)
            .map(char::from),
    )
}

/// The algorithm both readings share, over the letters each of them found.
///
/// Split from the letter test rather than parameterised by it because the test
/// is the whole of the difference between them: everything below counts and
/// codes letters identically, and only what *counts* as one changes.
fn soundex_of(mut letters: impl Iterator<Item = char>) -> String {
    // Zero for a vowel and for the letters that behave like one, and for any
    // letter outside ASCII: `CAFÉ` is `C100`, the accent contributing nothing
    // while still being a letter.
    let code = |c: char| match c.to_ascii_uppercase() {
        'B' | 'F' | 'P' | 'V' => b'1',
        'C' | 'G' | 'J' | 'K' | 'Q' | 'S' | 'X' | 'Z' => b'2',
        'D' | 'T' => b'3',
        'L' => b'4',
        'M' | 'N' => b'5',
        'R' => b'6',
        _ => b'0',
    };

    let Some(first) = letters.next() else {
        return String::new();
    };

    let mut out = String::new();
    out.push(first.to_ascii_uppercase());
    let mut last = code(first);
    for c in letters {
        let this = code(c);
        if this == b'0' {
            // A zero neither writes nor resets: the run carries across it.
            continue;
        }
        if this != last {
            out.push(this as char);
        }
        last = this;
    }
    while out.chars().count() < 4 {
        out.push('0');
    }
    out
}

/// A value with no fractional part, as an integer where one will hold it.
///
/// Not a bare `as i64`: that cast saturates in Rust, so `FLOOR(1e30)` answered
/// i64::MAX where MySQL answers 1e30. MySQL types these by their argument and
/// calls an approximate one a DOUBLE for exactly this reason — the answer is
/// whole, but it is not always an integer.
fn whole(n: f64) -> Value {
    if n >= i64::MIN as f64 && n < -(i64::MIN as f64) {
        Value::Int(n as i64)
    } else {
        Value::Float(n)
    }
}

/// Dispatches a scalar function call.
pub fn call(name: &str, args: Vec<Value>, session: &Session) -> EngineResult<Value> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
    let text = |i: usize| arg(i).as_text();

    // A NULL argument makes the answer NULL, before the function is reached.
    //
    // Checked here rather than in each function because the functions that got
    // it wrong were the ones that never thought about it: `UPPER(NULL)` read
    // the NULL as an empty string and answered `''`, so `WHERE UPPER(t) = ''`
    // matched rows MySQL does not return. A statement with no arguments at all
    // is unaffected, which is how `UNIX_TIMESTAMP()` keeps meaning now while
    // `UNIX_TIMESTAMP(NULL)` becomes NULL.
    if !args.is_empty()
        && args.iter().any(Value::is_null)
        && !NULL_TOLERANT.contains(&name)
        && KNOWN.binary_search(&name).is_ok()
    {
        return Ok(Value::Null);
    }

    Ok(match name {
        // -- dates and times ---------------------------------------------------
        //
        // `DATE_ADD`, `DATE_SUB` and `TIMESTAMPDIFF` are not here: their
        // arguments are an `INTERVAL` or a bare unit keyword rather than
        // values, so they are handled where the expression is still an AST.
        "hour" | "minute" | "second" | "microsecond" | "dayofweek" | "dayofyear" | "quarter"
        | "weekday" | "last_day" => {
            if arg(0).is_null() {
                return Ok(Value::Null);
            }
            // The clock extractors also take a bare TIME, which has no
            // calendar at all: `HOUR('10:30:00')` is 10 in MySQL, and a
            // `WHERE HOUR(t) = 9` over a TIME column would otherwise fail the
            // query rather than filter it.
            let clock_only = matches!(name, "hour" | "minute" | "second" | "microsecond");
            let parsed = temporal::parse(&text(0)).or_else(|| {
                clock_only
                    .then(|| temporal::parse_clock(&text(0)))
                    .flatten()
            });
            // A value MySQL cannot read is NULL and a warning, not a failed
            // statement — and the neighbouring YEAR/MONTH/DAY are lenient too,
            // so erroring here would make the same bad input behave two ways.
            let Some(stamp) = parsed else {
                return Ok(Value::Null);
            };
            match name {
                "hour" => Value::Int(i64::from(stamp.hour)),
                "minute" => Value::Int(i64::from(stamp.minute)),
                "second" => Value::Int(i64::from(stamp.second)),
                "microsecond" => Value::Int(i64::from(stamp.micros)),
                "dayofweek" => Value::Int(i64::from(stamp.day_of_week())),
                // WEEKDAY counts from Monday = 0, where DAYOFWEEK counts from
                // Sunday = 1. The two disagree on purpose and MySQL keeps both.
                "weekday" => Value::Int(i64::from((stamp.day_of_week() + 5) % 7)),
                "dayofyear" => Value::Int(i64::from(stamp.day_of_year())),
                "quarter" => Value::Int(i64::from((stamp.month - 1) / 3 + 1)),
                _ => {
                    let last_day = stamp
                        .add(temporal::Unit::Month, 1)
                        .map(|next| temporal::Stamp {
                            day: 1,
                            has_time: false,
                            ..next
                        })
                        .and_then(|first_of_next| first_of_next.add(temporal::Unit::Day, -1));
                    match last_day {
                        Some(day) => Value::Temporal(day.render()),
                        None => Value::Null,
                    }
                }
            }
        }
        "date_format" => {
            if arg(0).is_null() || arg(1).is_null() {
                return Ok(Value::Null);
            }
            match temporal::parse(&text(0)) {
                Some(stamp) => Value::text(temporal::format(&stamp, &text(1))?),
                None => Value::Null,
            }
        }
        "datediff" => {
            if arg(0).is_null() || arg(1).is_null() {
                return Ok(Value::Null);
            }
            // Whole days between the dates, ignoring the time of day, which is
            // what makes `DATEDIFF(x, NOW())` a count of days rather than of
            // 24-hour spans.
            match (temporal::parse(&text(0)), temporal::parse(&text(1))) {
                (Some(a), Some(b)) => Value::Int(
                    temporal::days_from_civil(a.year, a.month, a.day)
                        - temporal::days_from_civil(b.year, b.month, b.day),
                ),
                _ => Value::Null,
            }
        }

        // -- spatial -----------------------------------------------------------
        //
        // Only what the target codebase uses: a point, and the distance
        // between two of them. Held as WKT text, which is what `ST_AsText`
        // returns and so is a representation a client can already read.
        "point" => {
            if arg(0).is_null() || arg(1).is_null() {
                return Ok(Value::Null);
            }
            let (x, y) = (as_number(&arg(0), "point")?, as_number(&arg(1), "point")?);
            Value::text(format!("POINT({x} {y})"))
        }
        "st_distance_sphere" => {
            if arg(0).is_null() || arg(1).is_null() {
                return Ok(Value::Null);
            }
            let (a, b) = (read_point(&text(0))?, read_point(&text(1))?);
            Value::Float(distance_on_sphere(a, b))
        }

        // -- JSON -------------------------------------------------------------
        //
        // JSON is held as text and parsed here, where the structure is actually
        // needed. Text that is not JSON is an error rather than a NULL: a
        // column holding a truncated document is a bug, and answering NULL
        // hides it behind a result that looks merely empty.
        "json_extract" => {
            if arg(0).is_null() {
                return Ok(Value::Null);
            }
            let document = json::parse(&arg(0))?;

            // Several paths, or one path with a wildcard, produce an array;
            // a single path matching once produces the value itself.
            let mut found = Vec::new();
            let mut multiple = args.len() > 2;
            for path in &args[1..] {
                let parsed = json::parse_path(&path.as_text())?;
                multiple = multiple || parsed.is_multiple();
                found.extend(json::select(&document, &parsed).into_iter().cloned());
            }

            // "No path located a value" is NULL whether or not the paths
            // could have matched several — an empty array would make
            // `IS NULL` false where MySQL makes it true.
            match (multiple, found.len()) {
                (_, 0) => Value::Null,
                (false, _) => json::render(&found[0]),
                _ => json::render(&serde_json::Value::Array(found)),
            }
        }
        "json_unquote" => {
            if arg(0).is_null() {
                return Ok(Value::Null);
            }
            // Already-unquoted text is returned as it is, which is what makes
            // JSON_UNQUOTE(JSON_EXTRACT(...)) work whatever the extract found.
            match json::document_of(&arg(0)) {
                Ok(parsed) => json::unquote(&parsed),
                Err(_) => arg(0),
            }
        }
        // A yes-or-no answer, and MySQL types it as one: `JSON_ARRAY(
        // JSON_VALID('1'))` is `[true]` there. The same goes for the three
        // predicates below.
        "json_valid" => match arg(0) {
            Value::Null => Value::Null,
            other => {
                // Through the same parse a cast uses, so the answers agree:
                // `1e2000` is 0 here and 3141 there, and both because a
                // number a double cannot hold is not valid JSON text on 8.3.
                let valid = json::document_of(&other).is_ok();
                crate::expr::bool_value(valid)
            }
        },
        "json_quote" => {
            if arg(0).is_null() {
                return Ok(Value::Null);
            }
            // A string, not a document: JSON_QUOTE exists to produce the
            // *text* of a JSON string literal, so `JSON_OBJECT('q',
            // JSON_QUOTE('hi'))` is `{"q": "\"hi\""}` in MySQL. Tagging it
            // would quote it once and embed it unquoted, which is the one
            // context the function has.
            Value::text(serde_json::Value::String(text(0)).to_string())
        }
        "json_array" => {
            let items = args.iter().map(json::from_value).collect();
            json::render(&serde_json::Value::Array(items))
        }
        "json_object" => {
            if args.len() % 2 != 0 {
                return Err(EngineError::new(
                    1582,
                    "42000",
                    "Incorrect parameter count in the call to native function 'json_object'",
                ));
            }
            let mut object = serde_json::Map::new();
            for pair in args.chunks(2) {
                // A NULL key has nothing sensible to become, and MySQL says so.
                if pair[0].is_null() {
                    return Err(EngineError::new(
                        3158,
                        "22032",
                        "JSON documents may not contain NULL member names",
                    ));
                }
                object.insert(pair[0].as_text(), json::from_value(&pair[1]));
            }
            json::render(&serde_json::Value::Object(object))
        }
        "json_length" => {
            if arg(0).is_null() {
                return Ok(Value::Null);
            }
            let document = json::parse(&arg(0))?;
            let at = match args.get(1) {
                Some(path) => {
                    let parsed = json::parse_path(&path.as_text())?;
                    match json::select(&document, &parsed).first() {
                        Some(found) => (*found).clone(),
                        None => return Ok(Value::Null),
                    }
                }
                None => document,
            };
            Value::Int(match at {
                serde_json::Value::Array(items) => items.len() as i64,
                serde_json::Value::Object(map) => map.len() as i64,
                // A scalar is one value, not zero.
                _ => 1,
            })
        }
        "json_type" => {
            if arg(0).is_null() {
                return Ok(Value::Null);
            }
            // DECIMAL is missing from this list, and cannot be added while a
            // document is a rendering. MySQL has a third number type in the
            // binary form it stores JSON in, and #0071 gave the engine the
            // digits a decimal was written with — but the digits are not what
            // separates the types. Measured on 8.3, its *own* rendering is
            // byte-identical across the pairs it types differently:
            //
            //     DECIMAL(10,1) 2.5   ->  `2.5`  DECIMAL
            //     CAST('2.5' AS JSON) ->  `2.5`  DOUBLE
            //     DECIMAL(10,0) 2     ->  `2`    DECIMAL
            //     CAST('2' AS JSON)   ->  `2`    INTEGER
            //
            // So `Number::as_str` carries the spelling and the spelling is
            // not a function of the type. Answering DECIMAL for the decimals
            // that *are* separable — the ones with a trailing zero past the
            // shortest form, which no double spells — would make the answer
            // depend on the column's scale rather than on its type, and 8.3
            // says DECIMAL for both scales. Getting this right needs the type
            // carried beside the text, which is a change to what a
            // `Value::Json` is and to the image format that stores one.
            Value::text(match json::parse(&arg(0))? {
                serde_json::Value::Null => "NULL",
                serde_json::Value::Bool(_) => "BOOLEAN",
                serde_json::Value::Number(n) if n.is_f64() => "DOUBLE",
                serde_json::Value::Number(_) => "INTEGER",
                serde_json::Value::String(_) => "STRING",
                serde_json::Value::Array(_) => "ARRAY",
                serde_json::Value::Object(_) => "OBJECT",
            })
        }
        "json_keys" => {
            if arg(0).is_null() {
                return Ok(Value::Null);
            }
            match json::parse(&arg(0))? {
                serde_json::Value::Object(map) => {
                    let keys = map
                        .keys()
                        .map(|k| serde_json::Value::String(k.clone()))
                        .collect();
                    json::render(&serde_json::Value::Array(keys))
                }
                _ => Value::Null,
            }
        }
        "json_contains" => {
            if arg(0).is_null() || arg(1).is_null() {
                return Ok(Value::Null);
            }
            let document = json::parse(&arg(0))?;
            let candidate = json::parse(&arg(1))?;
            let target = match args.get(2) {
                Some(path) => {
                    let parsed = json::parse_path(&path.as_text())?;
                    match json::select(&document, &parsed).first() {
                        Some(found) => (*found).clone(),
                        None => return Ok(Value::Null),
                    }
                }
                None => document,
            };
            crate::expr::bool_value(json_contains(&target, &candidate))
        }
        "json_set" | "json_array_append" => {
            if arg(0).is_null() {
                return Ok(Value::Null);
            }
            if args.len() < 3 || (args.len() - 1) % 2 != 0 {
                return Err(EngineError::new(
                    1582,
                    "42000",
                    format!("Incorrect parameter count in the call to native function '{name}'"),
                ));
            }
            // A NULL *path* makes the whole call NULL, where a NULL *value* is
            // stored as a JSON null — which is why these cannot simply be
            // exempted from the rule above. Without this the NULL reaches
            // `parse_path` as an empty string and the statement fails with
            // 3143 where MySQL answers NULL.
            if args[1..].chunks(2).any(|pair| pair[0].is_null()) {
                return Ok(Value::Null);
            }
            let mut document = json::parse(&arg(0))?;
            for pair in args[1..].chunks(2) {
                let path = json::parse_path(&pair[0].as_text())?;
                let new = json::from_value(&pair[1]);
                if name == "json_set" {
                    json::set(&mut document, &path, new)?;
                } else {
                    json::append(&mut document, &path, new)?;
                }
            }
            json::render(&document)
        }
        "json_merge_patch" => {
            let mut merged = serde_json::Value::Null;
            for (i, argument) in args.iter().enumerate() {
                // A NULL argument makes the whole result NULL, as MySQL does.
                if argument.is_null() {
                    return Ok(Value::Null);
                }
                let parsed = json::parse(argument)?;
                merged = if i == 0 {
                    parsed
                } else {
                    json::merge_patch(&merged, &parsed)
                };
            }
            json::render(&merged)
        }

        // -- regular expressions ---------------------------------------------
        //
        // MySQL 8 uses ICU regex syntax and the `regex` crate uses Rust's;
        // they agree on everything the migrations use. A pattern one accepts
        // and the other does not is reported rather than quietly matching
        // nothing, which would silently rewrite the wrong rows.
        "regexp_replace" | "regexp_like" | "regexp_substr" | "regexp_instr" => {
            // MySQL's optional arguments sit at different positions per
            // function: a start position, which occurrence to act on, and a
            // match type. The migrations pass all three.
            let (pos_at, occurrence_at, return_at, match_at) = match name {
                "regexp_like" => (None, None, None, Some(2)),
                "regexp_replace" => (Some(3), Some(4), None, Some(5)),
                "regexp_substr" => (Some(2), Some(3), None, Some(4)),
                _ => (Some(2), Some(3), Some(4), Some(5)),
            };
            let fixed = if name == "regexp_replace" { 3 } else { 2 };
            if args.iter().take(fixed).any(|v| v.is_null()) {
                return Ok(Value::Null);
            }

            let subject = text(0);
            let at = |i: Option<usize>| i.and_then(|i| args.get(i)).cloned();
            let match_type = at(match_at).map(|v| v.as_text()).unwrap_or_default();
            let re = compile_regex(&text(1), &match_type)?;

            // Positions and offsets are in characters, as everything else in
            // MySQL's string functions is.
            let position = at(pos_at).and_then(|v| v.as_int()).unwrap_or(1);
            if position < 1 {
                return Err(EngineError::new(
                    1582,
                    "42000",
                    format!("Incorrect parameters in the call to native function '{name}'"),
                ));
            }
            let skipped: usize = subject
                .chars()
                .take(position as usize - 1)
                .map(char::len_utf8)
                .sum();
            let (prefix, hay) = subject.split_at(skipped.min(subject.len()));

            // 0 means every occurrence for REGEXP_REPLACE and the first for the
            // others, which is how MySQL reads the same default differently.
            let occurrence = at(occurrence_at)
                .and_then(|v| v.as_int())
                .unwrap_or(0)
                .max(0);
            let nth = |n: i64| -> Option<regex::Match<'_>> {
                re.find_iter(hay).nth(n.max(1) as usize - 1)
            };

            match name {
                "regexp_like" => crate::expr::bool_value(re.is_match(hay)),
                "regexp_replace" => {
                    let replaced = if occurrence == 0 {
                        re.replace_all(hay, text(2).as_str()).into_owned()
                    } else {
                        // Only the one occurrence is rewritten, so the match
                        // is replaced in place and the rest kept verbatim.
                        match re.captures_iter(hay).nth(occurrence.max(1) as usize - 1) {
                            Some(caps) => {
                                let whole = caps.get(0).expect("group 0 always matches");
                                let mut out = String::from(&hay[..whole.start()]);
                                caps.expand(text(2).as_str(), &mut out);
                                out.push_str(&hay[whole.end()..]);
                                out
                            }
                            None => hay.to_owned(),
                        }
                    };
                    Value::text(format!("{prefix}{replaced}"))
                }
                "regexp_substr" => match nth(occurrence) {
                    Some(m) => Value::text(m.as_str()),
                    None => Value::Null,
                },
                // REGEXP_INSTR reports a 1-based character position, 0 for no
                // match. A return option of 1 asks for the position after the
                // match rather than its start.
                _ => {
                    let after = at(return_at).and_then(|v| v.as_int()).unwrap_or(0) != 0;
                    Value::Int(nth(occurrence).map_or(0, |m| {
                        let edge = if after { m.end() } else { m.start() };
                        (prefix.chars().count() + hay[..edge].chars().count()) as i64 + 1
                    }))
                }
            }
        }

        // -- hashing --------------------------------------------------------
        //
        // The migrations use MD5 to derive stable ids from a seed value, so it
        // has to produce the same digest MySQL does, not merely some digest.
        "md5" => {
            use md5::Digest;
            if arg(0).is_null() {
                return Ok(Value::Null);
            }
            let digest = md5::Md5::digest(text(0).as_bytes());
            Value::text(
                digest
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
            )
        }

        // -- null handling ------------------------------------------------
        //
        // These aggregate the *types* of their arguments, so a temporal beside
        // anything else stops being one: measured on 8.3, `COALESCE(d, '0')`,
        // `COALESCE(d, 0)` and `COALESCE(d, 1.5)` are all VAR_STRING and all
        // read 2024 in arithmetic, where the bare `d` reads 20240105. `IF`,
        // `GREATEST`, `LEAST` and `CASE` were measured too and do *not*
        // aggregate that way — they keep the temporal — which is why the rule
        // is written here and not shared with them.
        //
        // An ENUM loses its ordinal here whatever it sits beside, which is not
        // the same rule and so is not folded into the same helper:
        // `COALESCE(e) + 0` is 0 on 8.3 with one argument and nothing to
        // aggregate with, where `IF(1, e, 'z') + 0` is still the ordinal.
        "coalesce" => {
            let first = args.iter().find(|v| !v.is_null()).cloned();
            let chosen =
                untag_mixed_temporal(&args, first.map(Value::untag_enum).map(Value::untag_bool));
            chosen
                .map(|v| untag_mixed_unsigned(&args, v))
                .unwrap_or(Value::Null)
        }
        "ifnull" | "nvl" => {
            let chosen = if arg(0).is_null() { arg(1) } else { arg(0) };
            let chosen = untag_mixed_temporal(&args, Some(chosen.untag_enum().untag_bool()));
            chosen
                .map(|v| untag_mixed_unsigned(&args, v))
                .unwrap_or(Value::Null)
        }
        // The boolean tag comes off here where the ENUM's ordinal does not,
        // which is why the two are not one call. Measured on 8.3,
        // `JSON_ARRAY(NULLIF(TRUE, 0))` and `JSON_ARRAY(IF(1, TRUE, FALSE))`
        // are both `[1]`, while `IF(1, e, 'z') + 0` is still the ordinal.
        "nullif" => {
            if arg(0).compare(&arg(1)) == Some(std::cmp::Ordering::Equal) {
                Value::Null
            } else {
                arg(0).untag_bool()
            }
        }
        // The condition is not a candidate, so only the two branches decide
        // the sign: `IF(iu, 1, 2)` is signed on 8.3 though its test is not.
        "if" => {
            let chosen = match arg(0).truthy() {
                Some(true) => arg(1).untag_bool(),
                _ => arg(2).untag_bool(),
            };
            untag_mixed_unsigned(args.get(1..).unwrap_or_default(), chosen)
        }
        "isnull" => crate::expr::bool_value(arg(0).is_null()),

        // -- strings -------------------------------------------------------
        "concat" => {
            // NULL poisons the whole concatenation, unlike CONCAT_WS.
            if args.iter().any(Value::is_null) {
                Value::Null
            } else {
                Value::Text(args.iter().map(Value::as_text).collect())
            }
        }
        "concat_ws" => {
            // Only the *values* are skipped. A NULL separator makes the whole
            // call NULL, which is why this cannot just be exempted from the
            // rule above wholesale: `CONCAT_WS(NULL, 'a', 'b')` is NULL, not
            // `'ab'`.
            if arg(0).is_null() {
                return Ok(Value::Null);
            }
            let sep = text(0);
            let joined: Vec<String> = args
                .iter()
                .skip(1)
                .filter(|v| !v.is_null())
                .map(Value::as_text)
                .collect();
            Value::Text(joined.join(&sep))
        }
        "length" | "octet_length" => Value::Int(text(0).len() as i64),
        "char_length" | "character_length" => Value::Int(text(0).chars().count() as i64),
        "upper" | "ucase" => Value::Text(text(0).to_uppercase()),
        "lower" | "lcase" => Value::Text(text(0).to_lowercase()),
        "trim" => Value::Text(text(0).trim().to_owned()),
        "ltrim" => Value::Text(text(0).trim_start().to_owned()),
        "rtrim" => Value::Text(text(0).trim_end().to_owned()),
        "reverse" => Value::Text(text(0).chars().rev().collect()),
        "space" => Value::Text(" ".repeat(arg(0).as_int().unwrap_or(0).max(0) as usize)),
        "replace" => Value::Text(text(0).replace(&text(1), &text(2))),
        "left" => {
            let n = arg(1).as_int().unwrap_or(0).max(0) as usize;
            Value::Text(text(0).chars().take(n).collect())
        }
        "right" => {
            let s = text(0);
            let n = arg(1).as_int().unwrap_or(0).max(0) as usize;
            let skip = s.chars().count().saturating_sub(n);
            Value::Text(s.chars().skip(skip).collect())
        }
        "substring" | "substr" | "mid" => {
            let s: Vec<char> = text(0).chars().collect();
            // MySQL positions are 1-based, and a negative start counts back
            // from the end.
            let start = arg(1).as_int().unwrap_or(1);
            let from = if start < 0 {
                s.len().saturating_sub(start.unsigned_abs() as usize)
            } else {
                (start.max(1) as usize) - 1
            };
            let take = match args.get(2) {
                Some(v) => v.as_int().unwrap_or(0).max(0) as usize,
                None => s.len(),
            };
            Value::Text(s.into_iter().skip(from).take(take).collect())
        }
        "instr" | "locate" | "position" => {
            let (haystack, needle) = if name == "instr" {
                (text(0), text(1))
            } else {
                (text(1), text(0))
            };
            // MySQL counts characters, not bytes: with any multi-byte text a
            // byte offset is a different, wrong position.
            Value::Int(
                haystack
                    .find(&needle)
                    .map_or(0, |byte| haystack[..byte].chars().count() as i64 + 1),
            )
        }
        "lpad" | "rpad" => {
            let s = text(0);
            let width = arg(1).as_int().unwrap_or(0).max(0) as usize;
            let pad = if args.len() > 2 {
                text(2)
            } else {
                " ".to_owned()
            };
            Value::Text(pad_to(&s, width, &pad, name == "lpad"))
        }

        // -- numbers -------------------------------------------------------
        // An integer keeps its exactness; everything else reads as the number
        // MySQL takes out of it, so `ABS('abc')` is 0 rather than NULL.
        // Untagged first, because a boolean is not a `Value::Int` and would
        // have fallen to the float arm: `ABS(TRUE)` is a LONGLONG 1 on 8.3,
        // and a double under that code renders `1` through the text protocol
        // while the binary one writes eight bytes of f64 — the split
        // `ResultSet::reconcile` exists to close, and it has no arm for this
        // pair. `ROUND`, `FLOOR`, `CEIL` and `TRUNCATE` need no such call:
        // they go through `whole`, which builds the integer from the number.
        //
        // The UNSIGNED reading survives, which is measured: `ABS(iu) - 100`
        // raises 1690 on 8.3 where `SUM(iu) - 100` answers -93, so `ABS` hands
        // the column's own reading back rather than building a new number.
        //
        // Checked rather than `i64::abs`, for the one value that has no
        // positive counterpart: MySQL raises 1690 for
        // `ABS(-9223372036854775808)` and Rust panicked on it.
        "abs" => match arg(0).untag_bool() {
            Value::Unsigned(v) => Value::Unsigned(v),
            Value::Int(v) => match v.checked_abs() {
                Some(v) => Value::Int(v),
                None => {
                    return Err(EngineError::new(
                        1690,
                        "22003",
                        "BIGINT value is out of range",
                    ))
                }
            },
            // A decimal keeps its scale through a sign, which is the same rule
            // `expr.rs` gives the unary minus: 1.50 rather than 1.5.
            Value::Decimal(ref text) => match crate::decimal::absolute(text) {
                Some(magnitude) => Value::Decimal(magnitude),
                None => Value::Float(crate::value::numeric_prefix(text).abs()),
            },
            other => Value::Float(other.as_number().abs()),
        },
        "round" => {
            let places = args.get(1).and_then(Value::as_int).unwrap_or(0);
            // An unsigned operand is rounded exactly, because an `f64` drops
            // every digit past 2^53 — which is most of a BIGINT UNSIGNED, and
            // is the difference between MySQL's answer and a number ending in
            // zeros that was never in the column.
            if let Value::Unsigned(v) = arg(0) {
                return unsigned_scaled(v, places, true);
            }
            let factor = 10f64.powi(places as i32);
            let rounded = (arg(0).as_number() * factor).round() / factor;
            if places <= 0 {
                whole(rounded)
            } else {
                Value::Float(rounded)
            }
        }
        // By the number MySQL reads out of the argument rather than a strict
        // parse: `FLOOR('abc')` is 0 and `CEIL('3.2')` is 4, where answering
        // NULL for anything unparseable made both of those NULL. A real NULL
        // has already been answered above.
        // An unsigned integer is already whole, and is answered without going
        // near the `f64` these otherwise round in.
        "floor" => match arg(0) {
            already @ Value::Unsigned(_) => already,
            other => whole(other.as_number().floor()),
        },
        "ceil" | "ceiling" => match arg(0) {
            already @ Value::Unsigned(_) => already,
            other => whole(other.as_number().ceil()),
        },
        // Through the operator it is spelled as, so the two cannot drift:
        // reading both sides as integers answered 1 for `MOD(1.500, 2)` under
        // a column described NEWDECIMAL, which promises the fraction it had
        // just thrown away.
        "mod" => {
            crate::expr::arithmetic(&sqlparser::ast::BinaryOperator::Modulo, &arg(0), &arg(1))?
        }
        // Dispatched on the argument rather than reading it as text: MySQL's
        // letter test comes from the argument's charset, and the binary one
        // admits only ASCII where a utf8mb4 one admits every codepoint from
        // U+00C0 up. Read as text, `SOUNDEX(CAST('éabc' AS BINARY))` answered
        // `é120` where MySQL answers `A120`.
        "soundex" => Value::text(match arg(0) {
            Value::Bytes(bytes) => soundex_binary(&bytes),
            other => soundex(&other.as_text()),
        }),
        // Toward zero at the given number of places, which may be negative:
        // `TRUNCATE(15.6, -1)` is 10. Not rounding — `TRUNCATE(1.567, 1)` is
        // 1.5 where `ROUND` would say 1.6.
        "truncate" => {
            let places = args.get(1).and_then(Value::as_int).unwrap_or(0);
            if let Value::Unsigned(v) = arg(0) {
                return unsigned_scaled(v, places, false);
            }
            let factor = 10f64.powi(places as i32);
            let cut = (arg(0).as_number() * factor).trunc() / factor;
            if places <= 0 {
                whole(cut)
            } else {
                Value::Float(cut)
            }
        }
        // The winner comes back untagged: `GREATEST(e, e) + 0` is 0 on 8.3
        // where the bare `e + 0` is the ordinal. `IF` and `CASE` were measured
        // beside it and do keep the tag, which is why this is written here.
        "greatest" | "least" => {
            if args.iter().any(Value::is_null) {
                Value::Null
            } else {
                let want_greater = name == "greatest";
                let candidates = args.clone();
                let winner = args
                    .into_iter()
                    .map(Value::untag_enum)
                    .map(Value::untag_bool)
                    .reduce(|a, b| {
                        let take_b = match a.compare(&b) {
                            Some(std::cmp::Ordering::Less) => want_greater,
                            Some(std::cmp::Ordering::Greater) => !want_greater,
                            _ => false,
                        };
                        if take_b {
                            b
                        } else {
                            a
                        }
                    })
                    .unwrap_or(Value::Null);
                untag_mixed_unsigned(&candidates, winner)
            }
        }

        // -- session and server --------------------------------------------
        "database" | "schema" => match &session.current_database {
            Some(db) => Value::Text(db.clone()),
            None => Value::Null,
        },
        "version" => Value::Text(session.config.version.clone()),
        "connection_id" => Value::Int(session.connection_id as i64),
        "last_insert_id" => Value::Int(session.last_insert_id as i64),
        "user" | "current_user" | "session_user" | "system_user" => {
            Value::Text("root@localhost".to_owned())
        }
        "row_count" => Value::Int(session.affected_rows as i64),

        // -- time ------------------------------------------------------------
        // The argument is the fractional-second precision, and ignoring it is
        // not cosmetic: a column declared `DATETIME(3)` filled by `NOW(3)`
        // would hold a second-truncated value, so two rows written in the same
        // second would compare equal when MySQL would order them.
        "now" | "current_timestamp" | "localtime" | "localtimestamp" | "sysdate" => {
            let digits = args
                .first()
                .and_then(|v| v.as_int())
                .unwrap_or(0)
                .clamp(0, 6);
            Value::Temporal(now_datetime_with(digits as usize))
        }
        "curdate" | "current_date" => Value::Temporal(now_datetime()[..10].to_owned()),
        // The argument is the fractional-second precision here too, and it is
        // read for the same reason `NOW(3)`'s is: the metadata reads it off the
        // token to type the column, so ignoring it described `CURTIME(3) + 0`
        // as a decimal and answered an integer.
        "curtime" | "current_time" => {
            let digits = args
                .first()
                .and_then(|v| v.as_int())
                .unwrap_or(0)
                .clamp(0, 6);
            Value::Temporal(now_datetime_with(digits as usize)[11..].to_owned())
        }
        "unix_timestamp" => Value::Int(unix_seconds() as i64),
        "date" => {
            let s = text(0);
            Value::Temporal(s.chars().take(10).collect())
        }
        // Through the same parser as the rest, so one bad value behaves one
        // way: splitting on '-' used to read '2026-13-45' as the year 2026
        // while QUARTER of the same text is NULL.
        "year" => date_part(&arg(0), |s| i64::from(s.year as u32)),
        "month" => date_part(&arg(0), |s| i64::from(s.month)),
        "day" | "dayofmonth" => date_part(&arg(0), |s| i64::from(s.day)),

        other => return Err(unsupported(format!("function `{other}()`"))),
    })
}

/// Drops the temporal tag where the arguments beside it were not temporals.
///
/// The aggregation is done over the argument *values*, because that is all
/// this function has. One case is out of reach that way: an argument that is
/// NULL carries no tag, so `COALESCE(d, s)` over a VARCHAR holding NULL keeps
/// the temporal where MySQL — which aggregates declared types, NULL or not —
/// makes it a string. Visible only in a further numeric context, and the same
/// limit `Value::Decimal` already has.
fn untag_mixed_temporal(args: &[Value], chosen: Option<Value>) -> Option<Value> {
    let chosen = chosen?;
    if !matches!(chosen, Value::Temporal(_)) {
        return Some(chosen);
    }
    let mixed = args
        .iter()
        .any(|v| !v.is_null() && !matches!(v, Value::Temporal(_)));
    Some(if mixed {
        Value::Text(chosen.as_text())
    } else {
        chosen
    })
}

/// `ROUND` or `TRUNCATE` of an unsigned integer, computed exactly.
///
/// A non-negative scale hands the number straight back: an integer is already
/// rounded there, and finding that out through an `f64` would lose the digits
/// the tag exists to carry.
///
/// A negative one cuts or rounds at a power of ten, in `u128` so that carrying
/// past the top of the range is visible rather than saturating. MySQL refuses
/// that carry rather than answering: `ROUND(bu, -1)` over 18446744073709551615
/// raises 1690 on 8.3, where `TRUNCATE(bu, -1)` answers 18446744073709551610
/// because cutting can only go down. `ROUND` goes half *up*, which is measured
/// — 12350 rounds to 12400 and 12345 to 12300.
///
/// A scale below -20 leaves nothing of a `u64`, so the answer is zero rather
/// than an overflowing power.
fn unsigned_scaled(v: u64, places: i64, round: bool) -> EngineResult<Value> {
    if places >= 0 {
        return Ok(Value::Unsigned(v));
    }
    let Some(step) = places
        .checked_neg()
        .and_then(|p| u32::try_from(p).ok())
        .and_then(|p| 10u128.checked_pow(p))
    else {
        return Ok(Value::Unsigned(0));
    };
    let value = u128::from(v);
    let cut = value - value % step;
    let scaled = if round && value - cut >= step.div_ceil(2) {
        cut + step
    } else {
        cut
    };
    u64::try_from(scaled)
        .map(Value::Unsigned)
        .map_err(|_| EngineError::new(1690, "22003", "BIGINT UNSIGNED value is out of range"))
}

/// Drops the UNSIGNED tag unless every candidate beside it carries one.
///
/// Shaped like `untag_mixed_temporal` above and measured separately from it,
/// because the two families are not the same list. This one is the same rule
/// the UNSIGNED flag on the wire follows, which is why they agree row for row:
/// `COALESCE(iu, iu) - 100` raises 1690 on 8.3 and reports UNSIGNED, while
/// `COALESCE(iu, 1) - 100` answers -93 and reports signed.
///
/// A NULL candidate does not count against it, which is measured — `IF(1, iu,
/// NULL)`, `COALESCE(NULL, iu)` and a `CASE` with no `ELSE` are all UNSIGNED
/// on 8.3 — and is the same blind spot `untag_mixed_temporal` documents: a
/// *column* holding NULL is signed by declaration and carries no tag to say
/// so.
///
/// The candidates are the arguments that could *be* the answer, which is not
/// always all of them: `IF`'s condition is not one, and `NULLIF` has only its
/// first, which is why that one does not call this at all.
fn untag_mixed_unsigned(candidates: &[Value], chosen: Value) -> Value {
    if candidates
        .iter()
        .all(|v| v.is_null() || matches!(v, Value::Unsigned(_)))
    {
        chosen
    } else {
        chosen.untag_unsigned()
    }
}

fn pad_to(s: &str, width: usize, pad: &str, left: bool) -> String {
    let len = s.chars().count();
    if len >= width || pad.is_empty() {
        return s.chars().take(width).collect();
    }
    let filler: String = pad.chars().cycle().take(width - len).collect();
    if left {
        format!("{filler}{s}")
    } else {
        format!("{s}{filler}")
    }
}

/// One field of a date, or NULL when the value is not a date at all.
fn date_part(value: &Value, field: impl Fn(&temporal::Stamp) -> i64) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    match temporal::parse(&value.as_text()) {
        Some(stamp) => Value::Int(field(&stamp)),
        None => Value::Null,
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
}

/// The current UTC time as `YYYY-MM-DD HH:MM:SS`.
///
/// Hand-rolled rather than pulling in a date crate: the engine needs one
/// format, and a dependency here would be paid on every cold start.
pub fn now_datetime() -> String {
    format_unix(unix_seconds())
}

/// `NOW(n)`: the current time with `n` fractional digits.
pub fn now_datetime_with(digits: usize) -> String {
    let whole = format_unix(unix_seconds());
    if digits == 0 {
        return whole;
    }
    let fraction = unix_nanos() % 1_000_000_000;
    // Truncated rather than rounded, which is what MySQL does — rounding could
    // carry into the next second and report a time that has not happened.
    let scaled = fraction / 10u64.pow(9 - digits as u32);
    format!("{whole}.{scaled:0width$}", width = digits)
}

/// Converts a Unix timestamp to `YYYY-MM-DD HH:MM:SS` using the civil-from-days
/// algorithm, which is correct across leap years and centuries.
pub fn format_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{year:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Applies a CAST.
pub fn cast(value: Value, ty: &DataType, source: &str) -> EngineResult<Value> {
    use DataType as D;
    if value.is_null() {
        return Ok(Value::Null);
    }
    Ok(match ty {
        D::TinyInt(_)
        | D::SmallInt(_)
        | D::Int(_)
        | D::Integer(_)
        | D::BigInt(_)
        | D::MediumInt(_)
        // MySQL spells the integer cast `SIGNED`, and it is by far the most
        // common one written by hand.
        | D::Signed
        | D::SignedInteger => Value::Int(int_prefix(&value)),
        // UNSIGNED reinterprets rather than refusing, which it could not do
        // while the engine held an `i64`: `CAST(-1 AS UNSIGNED)` is
        // 18446744073709551615 on 8.3 and `CAST(-2 AS UNSIGNED)` is
        // 18446744073709551614. This used to answer 1235.
        //
        // Read exactly rather than through `int_prefix`, which returns an
        // `i64` and so saturates on the values this cast exists to produce:
        // `CAST('18446744073709551615' AS UNSIGNED)` came back `i64::MAX`.
        //
        // A fractional argument truncates toward zero here where MySQL rounds
        // half away from zero — `CAST(-1.5 AS UNSIGNED)` is
        // 18446744073709551614 there and 18446744073709551615 here. That is
        // the signed cast's rounding rule above, which this one inherits by
        // sharing the reading, and it belongs to whichever issue takes it:
        // fixing it for one target and not the other would leave
        // `CAST(1.5 AS SIGNED)` and `CAST(1.5 AS UNSIGNED)` disagreeing about
        // the same number.
        D::Unsigned | D::UnsignedInteger => {
            let exact = value
                .as_exact_int()
                .unwrap_or_else(|| i128::from(int_prefix(&value)));
            Value::Unsigned(exact as u64)
        }
        // By the number MySQL reads out of the value: `CAST('abc' AS DOUBLE)`
        // is 0 there, not NULL. Answering NULL made the cast contradict the
        // NOT NULL a numeric target is described with.
        D::Double(_) | D::DoublePrecision => Value::Float(value.as_number()),
        // `CAST(x AS REAL)` follows the same flag a `REAL` column does: MySQL
        // describes it DOUBLE by default and FLOAT under REAL_AS_FLOAT, and
        // the value has to match whichever it announced.
        D::Real if !crate::exec::real_as_float() => Value::Float(value.as_number()),
        // A cast to FLOAT narrows, or the cast is only a relabelling: the
        // column is described FLOAT and a binary client decodes four bytes,
        // so the value has to be the one those four bytes hold.
        D::Float(_) | D::Real => {
            let n = value.as_number();
            let narrowed = n as f32;
            // `as f32` saturates, so `CAST(1e300 AS FLOAT)` came back an
            // infinity. MySQL refuses it, which is the answer that stays true.
            if narrowed.is_infinite() && n.is_finite() {
                return Err(EngineError::new(
                    1690,
                    "22003",
                    // Naming the operand the way MySQL does, which spells the
                    // cast in lower case and leaves the operand as written.
                    format!("DOUBLE value is out of range in 'cast({source} as float)'"),
                ));
            }
            Value::Float32(narrowed)
        }
        // Tagged and rescaled, exactly as a DECIMAL column's value is. The tag
        // is what carries the type past the cast: without it the result is an
        // ordinary string, and every later reader has to guess — a bitwise
        // operand read `CAST('3.7' AS DECIMAL(10,1))` by its leading digits
        // and answered 3 where MySQL rounds the decimal and answers 4.
        //
        // The scale comes with it, because the cast declares one and MySQL
        // applies it: `CAST('3.75' AS DECIMAL(10,1))` is 3.8 and a bare
        // `DECIMAL` is `DECIMAL(10,0)`, so `CAST('-2.5' AS DECIMAL)` is -3.
        // Handing the text back unrounded contradicted the NEWDECIMAL the
        // column is described as, at the scale it was described with.
        D::Decimal(_) | D::Numeric(_) | D::Dec(_) => {
            // Through `SqlType`, so the cast and a column declared the same
            // way fill an omitted scale in one place: a bare `DECIMAL` is
            // `DECIMAL(10,0)` to both.
            let target = crate::types::SqlType::from_ast(ty)?;
            // The same one-statement blow-up `check_declaration_limits` closes
            // for a column, reachable here by anyone who can send a SELECT:
            // the scale arrives as a `u64` straight from the parser, and
            // `decimal_digits` writes that many characters, so
            // `CAST(1.5 AS DECIMAL(10,1000000))` answered a million-digit
            // number and `DECIMAL(10,4294967295)` would ask for four
            // gigabytes.
            //
            // Its own rule, not the column's, and measured: a cast checks the
            // precision first, then M >= D, and only then the scale — so
            // `CAST(1.5 AS DECIMAL(70,40))` is 1426 where the column form is
            // 1425, and `CAST(1.5 AS DECIMAL(10,31))` is 1427 where the column
            // form is 1425. The operand stands in for the column's name, and
            // 1427 names no column at all.
            check_cast_precision(&target, source)?;
            let scale = target.scale.unwrap_or(0) as usize;
            // A temporal converts through its packed number, not its text:
            // `CAST(d AS DECIMAL(20,2))` is 20240105.00 in MySQL where reading
            // `2024-01-05` as a number gives 2024.00. An ENUM converts through
            // its ordinal for the same reason: `CAST(e AS DECIMAL(10,2))` is
            // 1.00 for the first member where reading `a` gives 0.00.
            let source = match &value {
                Value::Enum { ordinal, .. } => ordinal.to_string(),
                // The widened double rather than the six digits a float
                // *displays*, which is the argument #0071 already made for
                // `CAST(f AS JSON)` one arm below — the display is a
                // rendering, and converting from it makes the answer depend on
                // how wide the column happened to print. `CAST(f AS
                // DECIMAL(20,10))` over a FLOAT holding 1.234567890123 is
                // 1.2345678806 on 8.3 and was 1.2345700000 here, read back out
                // of `1.23457`.
                Value::Float32(v) => crate::decimal::plain(f64::from(*v)),
                _ => value
                    .temporal_number()
                    .map_or_else(|| value.as_text(), |number| number.as_text()),
            };
            Value::Decimal(crate::types::decimal_digits(&source, scale))
        }
        D::Char(_) | D::Varchar(_) | D::Text | D::CharacterVarying(_) | D::Character(_) => {
            Value::Text(value.as_text())
        }
        // Tagged, exactly as the DECIMAL cast above is and for the same
        // reason: the cast declares the type, and without the tag the result
        // is an ordinary string that every later reader has to guess at —
        // `CAST('2024-01-05' AS DATE) + 0` read 2024 where MySQL says
        // 20240105, while the bare literal beside it really is 2024.
        //
        // Written into the target's shape and to the precision the cast
        // declares: `CAST('06:07:08.567' AS TIME)` is 06:07:09 in MySQL and
        // `AS TIME(1)` is 06:07:08.6. Taking the first ten characters made
        // `AS DATE` a substring rather than a conversion, and keeping the text
        // verbatim made the declared precision a claim the value did not meet
        // — which is what the column is then described by.
        D::Date | D::Datetime(_) | D::Timestamp(_, _) | D::Time(_, _) => {
            let (shape, precision) = match ty {
                D::Date => (temporal::Shape::Date, 0),
                D::Time(precision, _) => (temporal::Shape::Time, precision.unwrap_or(0)),
                D::Datetime(precision) | D::Timestamp(precision, _) => {
                    (temporal::Shape::DateTime, precision.unwrap_or(0))
                }
                _ => unreachable!("guarded by this arm's own pattern"),
            };
            let text = value.as_text();
            Value::Temporal(temporal::normalise(&text, shape, precision as u32).unwrap_or(text))
        }
        // The cast is how a string becomes a document, so it parses rather
        // than relabelling: `JSON_OBJECT('c', CAST('[1,2]' AS JSON))` has to
        // give `{"c": [1, 2]}`, and text that is not JSON is an error here
        // exactly as it is on the way into a JSON column.
        // A boolean converts to the JSON boolean rather than being read as
        // the text `1`: `CAST(TRUE AS JSON)` is `true` on 8.3 and
        // `JSON_TYPE` of it is BOOLEAN, where `CAST(1 AS JSON)` is the number.
        // Every other value here is read as JSON *text*, which is why this is
        // one arm and not a call to `json::from_value` — `CAST('[1,2]' AS
        // JSON)` has to parse, not become a string.
        //
        // `CAST` is the only place the tag is honoured, and deliberately so:
        // it is the only one MySQL gives an answer for. A JSON *column* and a
        // JSON function argument both refuse a boolean there rather than
        // converting it — `INSERT INTO t(j) VALUES (TRUE)` and `UPDATE t SET
        // j = (1=1)` are 3140, `JSON_CONTAINS('[true]', TRUE)` is 3146 — and
        // they refuse the integer 1 in the same words, so honouring the tag
        // in `SqlType::coerce` or `json::parse` would invent an answer for a
        // statement MySQL will not run. That the engine accepts them at all
        // predates this and is the same gap for `Int`.
        D::JSON => match &value {
            Value::Bool(b) => json::render(&serde_json::Value::Bool(*b)),
            // An approximate number converts to the JSON number it *is*,
            // rather than being read back from the text it displays as. Both
            // halves of that were wrong. A decimal's text loses the scale —
            // `CAST(d AS JSON)` over a DECIMAL(10,2) holding 2.50 is `2.50`
            // on 8.3, and reading those digits back makes it the double they
            // spell, MySQL's parser having no decimal in it. A FLOAT's text
            // is the six significant digits the column *displays*, so a float
            // holding 1.234567890123 came back `1.23457` where 8.3 answers
            // `1.2345678806304932` — the widened double, and the same answer
            // `JSON_ARRAY(f)` gives here. Two paths disagreeing about one
            // value is worse than either being wrong. See #0071.
            Value::Decimal(_) | Value::Float(_) | Value::Float32(_) => {
                json::render(&json::from_value(&value))
            }
            _ => json::render(&json::parse(&value)?),
        },
        D::Binary(_) | D::Varbinary(_) | D::Blob(_) => Value::Bytes(value.as_text().into_bytes()),
        other => return Err(unsupported(format!("CAST to `{other}`"))),
    })
}

/// The limits MySQL puts on the `(M,D)` of a cast target.
///
/// Not the column's limits and not in the column's order — both measured on
/// 8.3, and both differ. Bounding all three is what keeps the scale below 30
/// and so keeps `decimal_digits` from being handed a length nothing checked.
fn check_cast_precision(target: &crate::types::SqlType, source: &str) -> EngineResult<()> {
    let (Some(precision), scale) = (target.length, target.scale.unwrap_or(0)) else {
        return Ok(());
    };
    if precision > 65 {
        return Err(EngineError::new(
            1426,
            "42000",
            format!("Too-big precision {precision} specified for '{source}'. Maximum is 65."),
        ));
    }
    // Before the scale limit, which is the half that is not the column's
    // order: `CAST(1.5 AS DECIMAL(10,31))` is 1427 and `DECIMAL(65,31)` is
    // 1425, so it is `M < D` that decides when both are wrong.
    if precision < scale {
        return Err(EngineError::new(
            1427,
            "42000",
            "For float(M,D), double(M,D) or decimal(M,D), M must be >= D (column '').",
        ));
    }
    if scale > 30 {
        return Err(EngineError::new(
            1425,
            "42000",
            format!("Too big scale {scale} specified for column '{source}'. Maximum is 30."),
        ));
    }
    Ok(())
}

/// MySQL LIKE matching: `%` is any run, `_` is one character.
///
/// Backtracking rather than regex, so there is no dependency and no pattern
/// compilation cost on a per-row call.
pub fn like(text: &str, pattern: &str, escape: Option<char>) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    like_at(&t, 0, &p, 0, escape)
}

fn like_at(t: &[char], mut ti: usize, p: &[char], mut pi: usize, escape: Option<char>) -> bool {
    while pi < p.len() {
        let ch = p[pi];

        if Some(ch) == escape {
            // An escaped character must match literally.
            //
            // A trailing escape has nothing to escape, and 8.3 matches it
            // against itself rather than failing the whole pattern:
            // `'a!' LIKE 'a!' ESCAPE '!'` is 1 there, and so is
            // `'a%' LIKE 'a%' ESCAPE '%'` — the wildcard reading is off even
            // in the position where the escaping cannot apply.
            let next = p.get(pi + 1).unwrap_or(&ch);
            if ti >= t.len() || t[ti] != *next {
                return false;
            }
            ti += 1;
            pi += 2;
            continue;
        }

        match ch {
            '%' => {
                // Try every split point; the shortest match wins first.
                for skip in ti..=t.len() {
                    if like_at(t, skip, p, pi + 1, escape) {
                        return true;
                    }
                }
                return false;
            }
            '_' => {
                if ti >= t.len() {
                    return false;
                }
                ti += 1;
                pi += 1;
            }
            literal => {
                if ti >= t.len() || t[ti] != literal {
                    return false;
                }
                ti += 1;
                pi += 1;
            }
        }
    }
    ti == t.len()
}

/// Whether `candidate` is contained in `target`, by MySQL's rules.
///
/// An object contains another when it has every one of its members; an array
/// contains a value when some element does. Anything else is equality.
///
/// Equality as *JSON* rather than as two trees, which is not the same
/// question for a number: `JSON_CONTAINS('[2.0]', '2')` is 1 on 8.3, and
/// `2.50` from a DECIMAL column is contained by an array holding `2.5`.
/// Comparing the parsed values instead made each spelling its own value —
/// visibly so once a number began keeping the digits it was written with.
fn json_contains(target: &serde_json::Value, candidate: &serde_json::Value) -> bool {
    use serde_json::Value as Json;
    match (target, candidate) {
        (Json::Object(a), Json::Object(b)) => b
            .iter()
            .all(|(k, v)| a.get(k).is_some_and(|found| json_contains(found, v))),
        (Json::Array(a), Json::Array(b)) => b
            .iter()
            .all(|v| a.iter().any(|found| json_contains(found, v))),
        (Json::Array(a), other) => a.iter().any(|found| json_contains(found, other)),
        (a, b) => json::compare(a, b) == Some(std::cmp::Ordering::Equal),
    }
}

/// The integer MySQL reads out of a value being cast.
///
/// MySQL takes the leading numeric part and warns about the rest, so
/// `CAST('12abc' AS SIGNED)` is 12 and `CAST('abc' AS SIGNED)` is 0. Returning
/// NULL for either would be a value the real server never produces.
fn int_prefix(value: &Value) -> i64 {
    if let Some(n) = value.as_int() {
        return n;
    }
    let text = value.as_text();
    let text = text.trim_start();
    let mut end = 0;
    for (i, c) in text.char_indices() {
        if c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')) {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    text[..end].parse().unwrap_or(0)
}

/// Compiles a pattern, reporting a bad one rather than treating it as literal
/// text.
///
/// MySQL matches case-insensitively for the usual collations, which is what a
/// `REGEXP` against a `utf8mb4_0900_ai_ci` column does, so that is the default
/// here. A match type of `c` or `i` overrides it; the others change semantics
/// this does not implement and are refused rather than ignored.
fn compile_regex(pattern: &str, match_type: &str) -> EngineResult<regex::Regex> {
    let mut case_insensitive = true;
    for flag in match_type.chars() {
        match flag {
            'c' => case_insensitive = false,
            'i' => case_insensitive = true,
            other => {
                return Err(unsupported(format!(
                    "the `{other}` match type of a regular expression"
                )))
            }
        }
    }

    regex::RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|e| {
            EngineError::new(
                3692,
                "HY000",
                format!("Illegal argument to a regular expression: {e}"),
            )
        })
}

/// A number from a value, for a function that has no sensible reading of text.
fn as_number(value: &Value, function: &str) -> EngineResult<f64> {
    value.as_float().ok_or_else(|| {
        EngineError::new(
            1367,
            "22007",
            format!("Illegal non-numeric value in the call to native function '{function}'"),
        )
    })
}

/// The longitude and latitude of a WKT point, in that order.
///
/// MySQL's `POINT(x, y)` takes longitude first, and `ST_Distance_Sphere`
/// validates both: latitude is inclusive at both ends, longitude is exclusive
/// at -180 and inclusive at 180. That asymmetry is real, and the error codes
/// are the ones an application catches.
fn read_point(text: &str) -> EngineResult<(f64, f64)> {
    let inner = text
        .trim()
        .strip_prefix("POINT(")
        .and_then(|rest| rest.strip_suffix(')'))
        .ok_or_else(|| {
            EngineError::new(
                3037,
                "22023",
                "Invalid GIS data provided to function st_distance_sphere.",
            )
        })?;

    let mut parts = inner.split_whitespace();
    let mut coordinate = || -> EngineResult<f64> {
        parts.next().and_then(|p| p.parse().ok()).ok_or_else(|| {
            EngineError::new(
                3037,
                "22023",
                "Invalid GIS data provided to function st_distance_sphere.",
            )
        })
    };
    let longitude = coordinate()?;
    let latitude = coordinate()?;

    if !(-180.0 < longitude && longitude <= 180.0) {
        return Err(EngineError::new(
            3616,
            "22S03",
            format!(
                "Longitude {longitude} is out of range in function st_distance_sphere. \
                 It must be within (-180.000000, 180.000000]."
            ),
        ));
    }
    if !(-90.0..=90.0).contains(&latitude) {
        return Err(EngineError::new(
            3617,
            "22S03",
            format!(
                "Latitude {latitude} is out of range in function st_distance_sphere. \
                 It must be within [-90.000000, 90.000000]."
            ),
        ));
    }
    Ok((longitude, latitude))
}

/// Great-circle distance in metres, by the haversine formula.
///
/// The radius is MySQL's own 6,370,986 m rather than a more modern figure, so
/// a distance computed here matches one computed there.
fn distance_on_sphere((lon_a, lat_a): (f64, f64), (lon_b, lat_b): (f64, f64)) -> f64 {
    const RADIUS: f64 = 6_370_986.0;

    let (phi_a, phi_b) = (lat_a.to_radians(), lat_b.to_radians());
    let delta_phi = (lat_b - lat_a).to_radians();
    let delta_lambda = (lon_b - lon_a).to_radians();

    let h = (delta_phi / 2.0).sin().powi(2)
        + phi_a.cos() * phi_b.cos() * (delta_lambda / 2.0).sin().powi(2);
    // `h` is at most 1 in exact arithmetic, but rounding the sum can push it a
    // couple of ulps past — and `asin` of anything above 1 is NaN, so a pair of
    // very nearly antipodal points would report a distance of NaN rather than
    // half the circumference. Clamping costs nothing and removes that entirely.
    2.0 * RADIUS * h.min(1.0).sqrt().asin()
}

#[cfg(test)]
mod known_names {
    use super::*;

    /// `KNOWN` must agree with the match, or the NULL rule stops firing for a
    /// function that has one and the engine answers `''` again.
    #[test]
    fn every_known_name_dispatches() {
        let session = Session::new(crate::session::ServerConfig::default(), 1);
        let mut missing = Vec::new();
        for name in KNOWN {
            // Two arguments is enough for every arity here; a function wanting
            // more reads `Value::Null` for the rest, which is not what is
            // under test.
            let args = vec![Value::Int(1), Value::Int(1)];
            if let Err(e) = call(name, args, &session) {
                if e.code == 1235 {
                    missing.push(*name);
                }
            }
        }
        assert!(
            missing.is_empty(),
            "listed in KNOWN but not dispatched: {missing:?}"
        );
    }

    #[test]
    fn the_list_is_sorted_because_it_is_searched_that_way() {
        let mut sorted = KNOWN.to_vec();
        sorted.sort_unstable();
        assert_eq!(KNOWN, sorted.as_slice());
    }

    /// The reason `KNOWN` exists: a function the engine does not implement has
    /// to say so, even when its argument is NULL and the rule would otherwise
    /// answer for it.
    ///
    /// `sha1` is a real MySQL function this engine has not got, which is the
    /// case that matters — a made-up name would pass whatever the rule did.
    /// It has to stay one that is absent: this read `soundex` until soundex
    /// was implemented, and then the test failed for being out of date rather
    /// than for finding anything. Any name missing from `KNOWN` will do.
    #[test]
    fn an_unknown_function_still_errors_on_a_null_argument() {
        let session = Session::new(crate::session::ServerConfig::default(), 1);
        for name in ["not_a_function", "sha1"] {
            assert!(
                KNOWN.binary_search(&name).is_err(),
                "{name} is implemented now; pick another absent one"
            );
            let err = call(name, vec![Value::Null], &session).unwrap_err();
            assert_eq!(err.code, 1235, "{name}");
        }
    }

    /// And every name that opts out of the rule has to be one that exists.
    #[test]
    fn every_tolerant_name_is_known() {
        for name in NULL_TOLERANT {
            assert!(KNOWN.binary_search(name).is_ok(), "{name} is not in KNOWN");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_handles_wildcards_and_anchoring() {
        assert!(like("hello", "hello", None));
        assert!(like("hello", "h%", None));
        assert!(like("hello", "%o", None));
        assert!(like("hello", "h_llo", None));
        assert!(like("hello", "%ell%", None));
        assert!(!like("hello", "h_lo", None));
        // A pattern with no wildcard must match the whole string.
        assert!(!like("hello world", "hello", None));
        assert!(like("", "%", None));
    }

    #[test]
    fn like_escape_makes_a_wildcard_literal() {
        assert!(like("100%", "100\\%", Some('\\')));
        assert!(!like("1000", "100\\%", Some('\\')));
        assert!(like("a_b", "a\\_b", Some('\\')));
        assert!(!like("axb", "a\\_b", Some('\\')));
    }

    /// Backtracking must not give up after the first `%` placement fails.
    #[test]
    fn like_backtracks_across_multiple_wildcards() {
        assert!(like("aaa", "%a%a%", None));
        assert!(!like("a", "%a%a%", None));
    }

    #[test]
    fn dates_convert_across_leap_years() {
        assert_eq!(format_unix(0), "1970-01-01 00:00:00");
        assert_eq!(format_unix(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(format_unix(1_709_164_800), "2024-02-29 00:00:00");
        assert_eq!(format_unix(1_735_689_599), "2024-12-31 23:59:59");
    }

    #[test]
    fn concat_is_null_poisoned_but_concat_ws_is_not() {
        let s = Session::new(crate::session::ServerConfig::default(), 1);
        let nulled = call("concat", vec![Value::text("a"), Value::Null], &s).unwrap();
        assert_eq!(nulled, Value::Null);

        let skipped = call(
            "concat_ws",
            vec![
                Value::text("-"),
                Value::text("a"),
                Value::Null,
                Value::text("b"),
            ],
            &s,
        )
        .unwrap();
        assert_eq!(skipped, Value::text("a-b"));
    }

    #[test]
    fn substring_is_one_based_and_accepts_a_negative_start() {
        let s = Session::new(crate::session::ServerConfig::default(), 1);
        let f = |args: Vec<Value>| call("substring", args, &s).unwrap();
        assert_eq!(
            f(vec![Value::text("abcdef"), Value::Int(2)]),
            Value::text("bcdef")
        );
        assert_eq!(
            f(vec![Value::text("abcdef"), Value::Int(2), Value::Int(3)]),
            Value::text("bcd")
        );
        assert_eq!(
            f(vec![Value::text("abcdef"), Value::Int(-2)]),
            Value::text("ef")
        );
    }

    #[test]
    fn an_unknown_function_is_an_error_rather_than_a_guess() {
        let s = Session::new(crate::session::ServerConfig::default(), 1);
        assert!(call("no_such_function", vec![], &s).is_err());
    }
}
