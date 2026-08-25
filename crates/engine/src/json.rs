//! JSON values and the path syntax that addresses into them.
//!
//! MySQL stores JSON as its own type and reaches into it with a path language
//! of its own — `$.a[0].b`, `$[*]`. The engine keeps JSON as text, because that
//! is how it arrives and leaves on the wire, and parses it where a function
//! actually needs the structure.
//!
//! The path subset here is what the target codebase uses: the root, member
//! names, array indexes, and the `[*]` wildcard. Anything else is an error
//! rather than a silent miss, because a path that quietly matches nothing looks
//! exactly like a column that is legitimately absent.

use serde_json::Value as Json;

use crate::error::{EngineError, EngineResult};
use crate::value::Value;

/// One step of a JSON path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `.name`
    Key(String),
    /// `[3]`
    Index(usize),
    /// `[*]`, every element of an array.
    Every,
}

/// A parsed `$`-rooted path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    steps: Vec<Step>,
}

impl Path {
    /// Whether the path can match more than one value, which is what decides
    /// between returning a value and returning an array of them.
    pub fn is_multiple(&self) -> bool {
        self.steps.contains(&Step::Every)
    }
}

fn bad_path(path: &str) -> EngineError {
    EngineError::new(
        3143,
        "42000",
        format!("Invalid JSON path expression '{path}'"),
    )
}

/// Parses `$`, `$.a`, `$[0]`, `$[*]`, and combinations of those.
pub fn parse_path(path: &str) -> EngineResult<Path> {
    let trimmed = path.trim();
    let mut chars = trimmed.chars().peekable();
    if chars.next() != Some('$') {
        return Err(bad_path(path));
    }

    let mut steps = Vec::new();
    loop {
        // MySQL allows space around each leg — `$. a` and `$ .a` are the same
        // path as `$.a`. Folding it into the member name makes a legal path
        // match nothing, which reads exactly like an absent key.
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(c) = chars.next() else { break };
        match c {
            '.' => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                // A quoted member allows the dots and brackets a bare one
                // cannot carry.
                if chars.peek() == Some(&'"') {
                    chars.next();
                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('"') => break,
                            Some(ch) => name.push(ch),
                            None => return Err(bad_path(path)),
                        }
                    }
                    steps.push(Step::Key(name));
                    continue;
                }

                let mut name = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch == '.' || ch == '[' {
                        break;
                    }
                    name.push(ch);
                    chars.next();
                }
                // Trailing space belongs to the separator, not the name.
                let name = name.trim_end().to_owned();
                if name.is_empty() {
                    return Err(bad_path(path));
                }
                steps.push(Step::Key(name));
            }
            '[' => {
                let mut inner = String::new();
                loop {
                    match chars.next() {
                        Some(']') => break,
                        Some(ch) => inner.push(ch),
                        None => return Err(bad_path(path)),
                    }
                }
                if inner.trim() == "*" {
                    steps.push(Step::Every);
                } else {
                    let index: usize = inner.trim().parse().map_err(|_| bad_path(path))?;
                    steps.push(Step::Index(index));
                }
            }
            _ => return Err(bad_path(path)),
        }
    }

    Ok(Path { steps })
}

/// Every value the path selects, in document order.
pub fn select<'a>(root: &'a Json, path: &Path) -> Vec<&'a Json> {
    let mut current = vec![root];
    for step in &path.steps {
        let mut next = Vec::new();
        for value in current {
            match step {
                Step::Key(name) => {
                    if let Some(found) = value.as_object().and_then(|o| o.get(name)) {
                        next.push(found);
                    }
                }
                Step::Index(i) => {
                    match value.as_array() {
                        Some(items) => next.extend(items.get(*i)),
                        // MySQL treats a scalar as a one-element array, so
                        // `$[0]` on a scalar is the scalar itself.
                        None if *i == 0 => next.push(value),
                        None => {}
                    }
                }
                Step::Every => match value.as_array() {
                    Some(items) => next.extend(items.iter()),
                    // A scalar or object is treated as a one-element array, the
                    // same auto-wrapping `[0]` does — without it a JSON_TABLE
                    // over an object yields no rows and the row vanishes.
                    None => next.push(value),
                },
            }
        }
        current = next;
    }
    current
}

/// Parses a value that is meant to hold JSON.
///
/// An engine `Value` reaches here as text, because that is how JSON is stored
/// and transmitted. Text that is not JSON is an error rather than a NULL: a
/// column holding `{"a":1` is a bug somewhere, and answering NULL hides it.
pub fn parse(value: &Value) -> EngineResult<Json> {
    if value.is_null() {
        return Ok(Json::Null);
    }
    let text = value.as_text();
    serde_json::from_str(&text).map_err(|e| {
        EngineError::new(
            3141,
            "22032",
            format!("Invalid JSON text: \"{e}\" in argument to a JSON function"),
        )
    })
}

/// Renders JSON back into the engine's representation, tagged as a document.
///
/// Tagged, so that feeding one JSON function's result to another embeds a
/// document rather than a string of its source text.
///
/// A JSON `null` renders as the text `null`, not as SQL NULL: "the key is
/// present and holds null" and "there is no such key" are different answers,
/// and collapsing them makes `IS NULL` true for both.
pub fn render(json: &Json) -> Value {
    Value::Json(render_text(json))
}

/// A document as MySQL writes it.
///
/// Not `serde_json::to_string`, which differs in two ways that both reach a
/// test as a different string. MySQL puts a space after every comma and every
/// colon; and it orders an object's keys by *byte length first*, then
/// bytewise, where serde_json's map is ordered lexicographically. So
/// `JSON_OBJECT('z',1,'a',2,'yy',3)` is `{"a": 2, "z": 1, "yy": 3}` there and
/// was `{"a":2,"yy":3,"z":1}` here — a different order, not just a different
/// spacing.
///
/// Strings are delegated to serde_json, whose escaping matches MySQL's —
/// checked against it, including `/`, `"` and `\`.
///
/// A double is *not* delegated — serde writes an exponent as `1e+30` where
/// MySQL writes `1e30`, and switches to exponent form earlier — so it goes
/// through [`double`] below, which follows MySQL's own rule. An integer is
/// delegated, because there is nothing to choose.
pub fn render_text(json: &Json) -> String {
    let mut out = String::new();
    write_json(&mut out, json);
    out
}

fn write_json(out: &mut String, json: &Json) {
    match json {
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json(out, item);
            }
            out.push(']');
        }
        Json::Object(members) => {
            let keys = stored_keys(members);
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&Json::String((*key).clone()).to_string());
                out.push_str(": ");
                write_json(out, &members[*key]);
            }
            out.push('}');
        }
        // A double is written by MySQL's rule rather than serde's, which
        // differ on both the threshold and the sign.
        Json::Number(n) if n.as_f64().is_some() && n.as_i64().is_none() && n.as_u64().is_none() => {
            out.push_str(&double(n.as_f64().unwrap_or(f64::NAN)));
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// A JSON double as MySQL writes one.
///
/// serde switches to exponent form earlier than MySQL and writes the sign —
/// `1e+30` where MySQL writes `1e30` — and Rust's own `Display` never switches
/// at all, so `1e30` comes out as thirty-one digits. Neither is the rule.
///
/// MySQL's is `my_gcvt`, which takes the fixed form when
///
/// ```text
/// decpt >= -14 && (decpt <= 15 || len > decpt)
/// ```
///
/// where `decpt` is where the point falls — one more than the exponent — and
/// `len` is how many digits the shortest round-trip needs. The two halves are
/// not symmetric, which is the part worth writing down: below, it is purely
/// the exponent, so `1e-15` is `0.000000000000001` and `1e-16` is `1e-16`.
/// Above, a value stays fixed past the threshold if its digits *reach past the
/// point* — `1e15` is `1e15` but `1000000000000000.5` is written out in full,
/// because seventeen digits do not fit in sixteen places. Treating the upper
/// bound as an exponent test alone is right for every whole power and wrong
/// for anything with a fraction between 1e15 and 4.5e15.
///
/// A whole number keeps a `.0` inside the fixed form, which is what makes a
/// document's `2.0` a double rather than the integer `2`.
fn double(value: f64) -> String {
    if !value.is_finite() {
        // Not representable in JSON at all; serde writes null and so does
        // this, rather than a token no parser reads.
        return "null".to_owned();
    }
    // `{:e}` is the shortest round-trip mantissa and a bare exponent, which is
    // already the spelling MySQL uses outside the fixed form.
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let decpt = exponent + 1;
    let len = mantissa.chars().filter(char::is_ascii_digit).count() as i32;

    if decpt < -14 || (decpt > 15 && len <= decpt) {
        return scientific;
    }
    let mut fixed = format!("{value}");
    if !fixed.contains('.') {
        fixed.push_str(".0");
    }
    fixed
}

/// The keys of an object in the order MySQL stores them: by byte length, then
/// bytewise.
///
/// Both the rendering and the comparison want this order, and they want the
/// *same* one — MySQL compares two objects key by key as stored, so
/// `{"b":1,"c":1}` is less than `{"aa":9,"c":1}` even though `aa` sorts before
/// `b` bytewise: stored, the second object begins with `c`.
fn stored_keys(members: &serde_json::Map<String, Json>) -> Vec<&String> {
    let mut keys: Vec<&String> = members.keys().collect();
    keys.sort_by(|a, b| {
        a.len()
            .cmp(&b.len())
            .then_with(|| a.as_bytes().cmp(b.as_bytes()))
    });
    keys
}

/// Where a document sits in MySQL's ordering of JSON types.
///
/// Ascending, and measured against 8.3 rather than taken from the manual,
/// which lists the precedences the other way up. A boolean sorting *after* an
/// array is the surprising one, and it is what MySQL does.
fn type_rank(json: &Json) -> u8 {
    match json {
        Json::Null => 0,
        Json::Number(_) => 1,
        Json::String(_) => 2,
        Json::Object(_) => 3,
        Json::Array(_) => 4,
        Json::Bool(_) => 5,
    }
}

/// Two documents compared as MySQL compares them.
///
/// Not as text. The engine stored JSON as its rendering and compared the
/// strings, which is wrong in both directions: `1` and `1.0` are the same
/// number and compared unequal, and the ordering came out alphabetical rather
/// than by JSON type.
///
/// `None` only for a number that is not comparable, which serde does not
/// produce from parsing.
pub fn compare(left: &Json, right: &Json) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;

    let rank = type_rank(left).cmp(&type_rank(right));
    if rank != Ordering::Equal {
        return Some(rank);
    }

    Some(match (left, right) {
        (Json::Null, Json::Null) => Ordering::Equal,
        (Json::Bool(a), Json::Bool(b)) => a.cmp(b),
        // Numerically, so `1` equals `1.0`. Exactly where both sides are
        // integers, because f64 cannot tell 9007199254740992 from the number
        // after it — sending both through it made two distinct documents
        // compare equal, which loses a DISTINCT row and rejects a legitimate
        // insert into a unique column.
        (Json::Number(a), Json::Number(b)) => {
            if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
                return Some(x.cmp(&y));
            }
            if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
                return Some(x.cmp(&y));
            }
            return a.as_f64()?.partial_cmp(&b.as_f64()?);
        }
        // Bytewise and case-sensitive, unlike a SQL string: `"Hi"` and `"hi"`
        // are two documents to MySQL and two rows to DISTINCT.
        (Json::String(a), Json::String(b)) => a.as_bytes().cmp(b.as_bytes()),
        (Json::Array(a), Json::Array(b)) => {
            for (x, y) in a.iter().zip(b) {
                match compare(x, y)? {
                    Ordering::Equal => {}
                    other => return Some(other),
                }
            }
            // A prefix is the smaller of the two.
            a.len().cmp(&b.len())
        }
        (Json::Object(a), Json::Object(b)) => {
            // The one with fewer keys first, whatever those keys are.
            match a.len().cmp(&b.len()) {
                Ordering::Equal => {}
                other => return Some(other),
            }
            // Member by member, key *then* that member's value — not every key
            // and then every value. The two disagree:
            // `{"a":9,"b":1}` and `{"a":1,"z":2}` share their first key, so the
            // 9 and the 1 decide and the left one is greater. Comparing all the
            // keys first would have reached `b` against `z` and called it
            // smaller.
            for (x, y) in stored_keys(a).into_iter().zip(stored_keys(b)) {
                match x.as_bytes().cmp(y.as_bytes()) {
                    Ordering::Equal => {}
                    other => return Some(other),
                }
                match compare(&a[x], &b[y])? {
                    Ordering::Equal => {}
                    other => return Some(other),
                }
            }
            Ordering::Equal
        }
        // The ranks matched, so the variants did.
        _ => Ordering::Equal,
    })
}

/// A string that is equal for exactly the documents [`compare`] calls equal.
///
/// DISTINCT, GROUP BY and uniqueness all key on a string rather than a
/// comparison, so they need one that agrees. The rendering nearly does — MySQL
/// normalises a document on the way in, so key order and spacing are already
/// settled — but it writes `1` and `1.0` differently where the comparison
/// calls them the same number.
pub fn comparison_key(json: &Json) -> String {
    let mut out = String::new();
    write_comparison_key(&mut out, json);
    out
}

/// A number written so that exactly the numbers [`compare`] calls equal share
/// a spelling.
///
/// Whole numbers key as integers however they were written, so `1` and `1.0`
/// meet and `-0.0` meets `0` — and a big integer keys as itself rather than as
/// the f64 it does not fit in.
fn number_key(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(f64::NAN);
    // A whole double keys as the integer it is, which is what makes `1.0` meet
    // `1` — and folds `-0.0` into `0`, where printing the double would have
    // split a value the comparison calls equal.
    if f.fract() == 0.0 && f.abs() < i64::MAX as f64 {
        return (f as i64).to_string();
    }
    format!("{f:?}")
}

fn write_comparison_key(out: &mut String, json: &Json) {
    out.push((b'0' + type_rank(json)) as char);
    match json {
        Json::Number(n) => out.push_str(&number_key(n)),
        Json::Array(items) => {
            out.push('[');
            for item in items {
                write_comparison_key(out, item);
                out.push(',');
            }
            out.push(']');
        }
        Json::Object(members) => {
            out.push('{');
            for key in stored_keys(members) {
                // Length-prefixed, so `{"ab":…}` and `{"a":"b…"}` cannot
                // produce the same key by running together.
                out.push_str(&format!("{}:{key}=", key.len()));
                write_comparison_key(out, &members[key]);
                out.push(',');
            }
            out.push('}');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// A JSON value as SQL sees it once the quotes come off.
///
/// `JSON_UNQUOTE` and `->>` both want this: a JSON string becomes the string,
/// and everything else keeps its JSON rendering.
pub fn unquote(json: &Json) -> Value {
    match json {
        Json::String(s) => Value::text(s),
        // JSON null unquotes to the text `null`, as MySQL does — it is a value
        // that was there, not a value that was missing. Anything else keeps
        // its document form, written the same way `render_text` writes one:
        // `JSON_UNQUOTE` on an object is still that object's text, and having
        // it spelled compactly here put two spellings in one row for
        // `SELECT doc, doc->>'$.a'`.
        other => Value::text(render_text(other)),
    }
}

/// An engine value as JSON, for building documents.
///
/// Text that happens to parse as JSON is *not* treated as JSON: MySQL keeps a
/// string a string unless it is cast, and guessing would turn a column holding
/// `"123"` into a number. A JSON-tagged value is a document and is embedded as
/// one, which is the whole reason the tag exists — `JSON_OBJECT('a',
/// JSON_ARRAY(1, 2))` gives `{"a": [1, 2]}` rather than `{"a": "[1,2]"}`.
pub fn from_value(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Int(n) => Json::from(*n),
        Value::Float(f) => serde_json::Number::from_f64(*f).map_or(Json::Null, Json::Number),
        // Widened, not through the six digits a FLOAT column displays:
        // MySQL's JSON number is a double, and `JSON_OBJECT('a', f)` over a
        // float holding 1.234567890123 gives `{"a": 1.2345678806304932}`.
        Value::Float32(f) => {
            serde_json::Number::from_f64(f64::from(*f)).map_or(Json::Null, Json::Number)
        }
        Value::Text(s) => Json::String(s.clone()),
        // A string, like the text it renders as: MySQL gives
        // `JSON_OBJECT('a', d)` the value `"2024-01-05"` and not the packed
        // number it would read in arithmetic. The tag changes the *numeric*
        // reading of a temporal and nothing else.
        Value::Temporal(s) => Json::String(s.clone()),
        // The member, for the same reason: `JSON_OBJECT('a', e)` is
        // `{"a": "b"}` on 8.3 and not the ordinal. The tag changes the
        // *numeric* reading of an ENUM and nothing else.
        Value::Enum { member, .. } => Json::String(member.clone()),
        // The one place the boolean tag is visible at all. MySQL converts the
        // keyword to the JSON boolean and the literal 1 to the JSON number, so
        // `CAST('true' AS JSON) = TRUE` is 1 and `= 1` is 0 — and the same for
        // every comparison and logical operator: `JSON_ARRAY(1 = 1)` is
        // `[true]` there, not `[1]`.
        Value::Bool(b) => Json::Bool(*b),
        Value::Bytes(b) => Json::String(String::from_utf8_lossy(b).into_owned()),
        // A number rather than a string: `JSON_OBJECT('a', decimal_column)`
        // gives `{"a": 2.5}` and not `{"a": "2.50"}`. The scale is lost on the
        // way through, because `serde_json` is built without
        // `arbitrary_precision` and reads every number into an f64 — MySQL
        // keeps it and answers `{"a": 2.50}`. See #0071.
        Value::Decimal(s) => s
            .parse::<serde_json::Number>()
            .map_or_else(|_| Json::String(s.clone()), Json::Number),
        // Every route to a tagged value validates it first — a constructor
        // serialised it from a `Json`, a column coerced it, an image checked
        // it on load — so this parses what one of those wrote. The fallback is
        // unreachable rather than lenient; a JSON null beats re-encoding the
        // source text as a string, which is the double-encoding this removes.
        Value::Json(s) => serde_json::from_str(s).unwrap_or(Json::Null),
    }
}

/// Replaces or inserts the value at a path, as `JSON_SET` does.
///
/// Only paths that name one place can be written to, which is MySQL's rule
/// too: `$[*]` selects many and there is no sensible single write.
pub fn set(root: &mut Json, path: &Path, new: Json) -> EngineResult<()> {
    if path.is_multiple() {
        return Err(EngineError::new(
            3149,
            "42000",
            "In this situation, path expressions may not contain the * token",
        ));
    }

    let Some((last, leading)) = path.steps.split_last() else {
        *root = new;
        return Ok(());
    };

    let mut current = root;
    for step in leading {
        current = match step {
            Step::Key(name) => match current {
                Json::Object(map) => map.entry(name.clone()).or_insert(Json::Null),
                // A path through something that is not an object matches
                // nothing, and MySQL leaves the document alone.
                _ => return Ok(()),
            },
            Step::Index(i) => match current {
                Json::Array(items) if *i < items.len() => &mut items[*i],
                _ => return Ok(()),
            },
            Step::Every => unreachable!("rejected above"),
        };
    }

    match last {
        Step::Key(name) => {
            if let Json::Object(map) = current {
                map.insert(name.clone(), new);
            }
        }
        Step::Index(i) => match current {
            Json::Array(items) => {
                if *i < items.len() {
                    items[*i] = new;
                } else {
                    items.push(new);
                }
            }
            // Setting `$[0]` on a scalar replaces it, which is what treating a
            // scalar as a one-element array implies.
            other if *i == 0 => *other = new,
            _ => {}
        },
        Step::Every => unreachable!("rejected above"),
    }
    Ok(())
}

/// Appends to the array at a path, as `JSON_ARRAY_APPEND` does.
///
/// A path naming something that is not an array wraps it into one first, which
/// is MySQL's behaviour rather than an error.
pub fn append(root: &mut Json, path: &Path, new: Json) -> EngineResult<()> {
    if path.is_multiple() {
        return Err(EngineError::new(
            3149,
            "42000",
            "In this situation, path expressions may not contain the * token",
        ));
    }

    let target = select(root, path).first().map(|v| (*v).clone());
    let Some(existing) = target else {
        return Ok(());
    };
    let appended = match existing {
        Json::Array(mut items) => {
            items.push(new);
            Json::Array(items)
        }
        scalar => Json::Array(vec![scalar, new]),
    };
    set(root, path, appended)
}

/// Merges `patch` into `target` by RFC 7386, which is `JSON_MERGE_PATCH`.
pub fn merge_patch(target: &Json, patch: &Json) -> Json {
    match patch {
        Json::Object(patch_map) => {
            let mut merged = match target {
                Json::Object(map) => map.clone(),
                // Anything that is not an object is replaced wholesale before
                // the patch is applied.
                _ => serde_json::Map::new(),
            };
            for (key, value) in patch_map {
                if value.is_null() {
                    // Null in a patch means remove, not set to null.
                    merged.remove(key);
                } else {
                    let existing = merged.get(key).cloned().unwrap_or(Json::Null);
                    merged.insert(key.clone(), merge_patch(&existing, value));
                }
            }
            Json::Object(merged)
        }
        other => other.clone(),
    }
}
