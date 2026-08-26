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

use serde_json::{Number, Value as Json};

use crate::error::{EngineError, EngineResult};
use crate::value::Value;

/// MySQL's own words for a number a double cannot hold, which is the one
/// parse failure serde no longer raises for us — see [`normalise`].
const TOO_BIG: &str = "Number too big to be stored in double.";

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
    document_of(value).map_err(|e| {
        EngineError::new(
            3141,
            "22032",
            format!("Invalid JSON text: \"{e}\" in argument to a JSON function"),
        )
    })
}

/// The document a value holds, from whichever side of the boundary it came.
///
/// The two sides are not the same parse, and the difference is the whole of
/// #0071. MySQL's JSON parser has no decimal in it: source text `2.50` is the
/// double 2.5 and `JSON_TYPE` calls it DOUBLE. The same digits arriving from a
/// `DECIMAL` column are a decimal, keep their scale, and are written back as
/// `2.50`. So text is normalised on the way in and a document already tagged
/// as one is taken exactly as written — which is what carries the scale across
/// the text this engine stores a document as.
///
/// `Err` carries the parser's own message, for the caller to place in whatever
/// wording its error code wants.
pub fn document_of(value: &Value) -> Result<Json, String> {
    match value {
        Value::Json(text) => parse_document(text),
        other => parse_source(&other.as_text()),
    }
}

/// Parses source text into a document, reading each number as MySQL reads one.
///
/// Not public: every caller outside this module is on one side of the
/// boundary or the other and wants [`document_of`] to pick for it.
fn parse_source(text: &str) -> Result<Json, String> {
    let mut parsed = read(text)?;
    normalise(&mut parsed)?;
    Ok(parsed)
}

/// Parses this engine's own rendering of a document back into one.
///
/// Verbatim, because every number in it was already spelled the way MySQL
/// spells one before it was written out.
pub fn parse_document(text: &str) -> Result<Json, String> {
    read(text)
}

/// The one place JSON text becomes a tree.
///
/// Through [`Document`] rather than `serde_json::Value`, because with
/// `arbitrary_precision` on, serde_json's own reader treats one object key as
/// a private protocol — see there.
fn read(text: &str) -> Result<Json, String> {
    serde_json::from_str::<Document>(text)
        .map(|Document(json)| json)
        .map_err(|e| e.to_string())
}

/// The key serde_json reserves for a number it is holding as text.
///
/// Not a key to MySQL, which stores it like any other — measured on 8.3, all
/// four of `{"$serde_json::private::Number": "1"}`, `: 1`, `: "abc"` and
/// `JSON_OBJECT('$serde_json::private::Number', 1)` are objects of length one.
const NUMBER_TOKEN: &str = "$serde_json::private::Number";

/// A document read without serde_json's number sentinel getting in the way.
///
/// With `arbitrary_precision` on, serde_json passes a number it is holding as
/// text through the visitor as a one-member map keyed by [`NUMBER_TOKEN`], and
/// its own `Value` reader turns any map with that key back into a number. So
/// a document that legitimately *has* that key stopped being itself: the text
/// `{"$serde_json::private::Number": "1"}` became the number 1, `: 1` and
/// `: "abc"` became parse errors, and a row holding one — which this engine's
/// own `JSON_OBJECT` can build — failed the whole state image on reload.
///
/// The two are distinguishable, and by something firmer than a guess about
/// the text. serde_json hands a real JSON string to the visitor *borrowed from
/// the input*, or from its unescaping scratch buffer; it hands a number's
/// digits over as an owned `String`, because they come from the `String` it
/// parked them in. So `visit_string` means "serde's own number" and
/// `visit_str` means "a member whose value is a string" — see [`Sentinel`],
/// and the test that pins both directions so a change in either fails loudly
/// rather than quietly.
struct Document(Json);

/// What followed [`NUMBER_TOKEN`]: serde's parked digits, or a real member's
/// value.
enum Sentinel {
    /// Owned, so serde_json parked it — this map is a number.
    Digits(String),
    /// Anything else, so the key is a key and this is its value.
    Member(Json),
}

struct DocumentVisitor;

impl<'de> serde::de::Visitor<'de> for DocumentVisitor {
    type Value = Document;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Document, E> {
        Ok(Document(Json::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Document, E> {
        Ok(Document(Json::from(v)))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Document, E> {
        Ok(Document(Json::from(v)))
    }

    // Both spellings of a string reach here — serde's default `visit_borrowed_str`
    // forwards to this one — and `visit_string` below has to say the same
    // thing, because a *string* is a string however it was delivered. Only
    // [`Sentinel`] reads anything into the difference.
    fn visit_str<E>(self, v: &str) -> Result<Document, E> {
        Ok(Document(Json::String(v.to_owned())))
    }

    fn visit_string<E>(self, v: String) -> Result<Document, E> {
        Ok(Document(Json::String(v)))
    }

    fn visit_unit<E>(self) -> Result<Document, E> {
        Ok(Document(Json::Null))
    }

    fn visit_seq<A>(self, mut items: A) -> Result<Document, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut array = Vec::new();
        while let Some(Document(item)) = items.next_element()? {
            array.push(item);
        }
        Ok(Document(Json::Array(array)))
    }

    fn visit_map<A>(self, mut members: A) -> Result<Document, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let Some(key) = members.next_key::<String>()? else {
            return Ok(Document(Json::Object(serde_json::Map::new())));
        };
        // Only the *first* key can be serde's, because that is the only shape
        // it produces; a token further along is a key like any other and the
        // loop below takes it.
        let first = if key == NUMBER_TOKEN {
            match members.next_value::<Sentinel>()? {
                Sentinel::Digits(digits) => {
                    let number = digits.parse::<Number>().map_err(serde::de::Error::custom)?;
                    return Ok(Document(Json::Number(number)));
                }
                Sentinel::Member(value) => value,
            }
        } else {
            members.next_value::<Document>()?.0
        };

        let mut object = serde_json::Map::new();
        object.insert(key, first);
        // A repeated key keeps the last value, which is serde_json's own rule
        // and 8.3's: `CAST('{"a":1,"a":2}' AS JSON)` is `{"a": 2}` there.
        while let Some((key, Document(value))) = members.next_entry::<String, Document>()? {
            object.insert(key, value);
        }
        Ok(Document(Json::Object(object)))
    }
}

impl<'de> serde::Deserialize<'de> for Document {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(DocumentVisitor)
    }
}

struct SentinelVisitor;

impl<'de> serde::de::Visitor<'de> for SentinelVisitor {
    type Value = Sentinel;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    /// Borrowed from the input or unescaped into serde's scratch buffer:
    /// either way it is a string that was written in the document.
    fn visit_str<E>(self, v: &str) -> Result<Sentinel, E> {
        Ok(Sentinel::Member(Json::String(v.to_owned())))
    }

    /// Owned, which the JSON reader never produces for a string it read —
    /// this is serde_json handing back the digits it parked.
    fn visit_string<E>(self, v: String) -> Result<Sentinel, E> {
        Ok(Sentinel::Digits(v))
    }

    fn visit_bool<E>(self, v: bool) -> Result<Sentinel, E> {
        Ok(Sentinel::Member(Json::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Sentinel, E> {
        Ok(Sentinel::Member(Json::from(v)))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Sentinel, E> {
        Ok(Sentinel::Member(Json::from(v)))
    }

    fn visit_unit<E>(self) -> Result<Sentinel, E> {
        Ok(Sentinel::Member(Json::Null))
    }

    fn visit_seq<A>(self, items: A) -> Result<Sentinel, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        DocumentVisitor
            .visit_seq(items)
            .map(|Document(json)| Sentinel::Member(json))
    }

    fn visit_map<A>(self, members: A) -> Result<Sentinel, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        DocumentVisitor
            .visit_map(members)
            .map(|Document(json)| Sentinel::Member(json))
    }
}

impl<'de> serde::Deserialize<'de> for Sentinel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(SentinelVisitor)
    }
}

/// Rewrites every number of a freshly parsed document the way MySQL's parser
/// reads one.
///
/// With `arbitrary_precision` a `Number` holds the text it was written as, so
/// without this a parsed `2.50` would print itself straight back and be
/// indistinguishable from the decimal `2.50` that has to. Measured on 8.3:
/// `CAST('2.50' AS JSON)` is `2.5` and DOUBLE, `CAST('-0' AS JSON)` is the
/// integer `0`, and `CAST('18446744073709551616' AS JSON)` — past what an
/// integer holds — is the double `1.8446744073709552e19`.
///
/// It is also where a number too big for a double is refused. Without the
/// feature serde did that itself, reading the literal into an f64 and failing
/// on the infinity; with it the digits are kept and nothing objects, so
/// `1e2000` would have become a valid document here and is 3141 on 8.3.
fn normalise(json: &mut Json) -> Result<(), String> {
    match json {
        Json::Number(n) => *n = as_parsed(n)?,
        Json::Array(items) => {
            for item in items {
                normalise(item)?;
            }
        }
        Json::Object(members) => {
            for member in members.values_mut() {
                normalise(member)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn as_parsed(n: &Number) -> Result<Number, String> {
    // An integer literal that fits an integer stays one, as written — and
    // `-0` reaches `0` through the i64, which is the answer 8.3 gives.
    if !n.as_str().contains(['.', 'e', 'E']) {
        if let Some(i) = n.as_i64() {
            return Ok(i.into());
        }
        if let Some(u) = n.as_u64() {
            return Ok(u.into());
        }
    }
    // Everything else is a double, spelled by [`double`] rather than kept as
    // it was typed: `1E2` is `100.0` on 8.3 and not `1E2`.
    //
    // Through serde's own reading of the digits and *not* `Number::as_f64`,
    // which is Rust's. The two do not always land on the same double, and the
    // one that matches MySQL is serde's: both it and the rapidjson MySQL
    // parses JSON with take the fast, approximate path by default, so
    // `1000000000000000.5` is the double below on both and the one above in
    // Rust. Measured on 8.3 — it answers 1000000000000000.4, and reading that
    // literal exactly would answer .5. Where the two parsers *do* differ is
    // #0072, which this leaves exactly where it was.
    //
    // Reading it that way is also the refusal: serde answers "number out of
    // range" rather than an infinity, which is the check `normalise` promises
    // and 8.3's own answer to `1e2000`.
    let value: f64 = serde_json::from_str(n.as_str()).map_err(|_| TOO_BIG.to_owned())?;
    double_number(value).ok_or_else(|| TOO_BIG.to_owned())
}

/// A double as a JSON number, spelled the way MySQL spells one.
///
/// `None` for a value JSON has no number for, which is what [`double`] writes
/// as `null` and no `Number` parses.
fn double_number(value: f64) -> Option<Number> {
    double(value).parse().ok()
}

/// A number as it is written out, which is how it is held but for one thing.
///
/// serde canonicalises an exponent's sign on the way in — `1e15` and `1E2` are
/// held as `1e+15` and `1e+2`, whichever way they were typed — and MySQL
/// writes no sign there. Everything else about the text survives, the mantissa
/// and a decimal's trailing zeros included, so this is the whole of the
/// difference between what is held and what goes out.
fn written(n: &Number) -> String {
    let text = n.as_str();
    // Only the exponent marker can carry a `+` in a JSON number, so the first
    // one is the one.
    match text.split_once("e+") {
        Some((mantissa, exponent)) => format!("{mantissa}e{exponent}"),
        None => text.to_owned(),
    }
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
/// A number is written exactly as the document holds it. Every number in one
/// is spelled before it gets here — a parsed one by [`normalise`], a built one
/// by [`from_value`] — so that a decimal's trailing zeros survive to be
/// written and a double is already in MySQL's spelling rather than serde's,
/// which writes an exponent as `1e+30` where MySQL writes `1e30` and switches
/// to exponent form earlier.
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
        // As held. Re-spelling it here would undo the two things the holding
        // is for: a decimal's scale, and a double already put in MySQL's form.
        Json::Number(n) => out.push_str(&written(n)),
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
        // Numerically, so `1` equals `1.0` and the decimal `2.50` equals the
        // double `2.5` — and exactly, over the digits, because f64 cannot tell
        // 9007199254740992 from the number after it, nor two `DECIMAL(30,2)`s
        // apart at their twentieth digit. Sending those through one made two
        // distinct documents compare equal, which loses a DISTINCT row and
        // rejects a legitimate insert into a unique column. Measured: 8.3
        // answers 0 for both pairs, and 0 for `1e30` against
        // 1000000000000000000000000000001.
        (Json::Number(a), Json::Number(b)) => {
            if let (Some(x), Some(y)) = (exact(a), exact(b)) {
                if let Some(order) = crate::decimal::compare(&x, &y) {
                    return Some(order);
                }
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

/// A number's digits with any exponent written out, which is the form both the
/// exact comparison and the key work in.
///
/// A double is read through the shortest digits that spell it rather than
/// through the value it actually holds — measured, 8.3 calls the JSON `0.1`
/// equal to the decimal `0.1` and *not* to 0.100000000000000005551115123126,
/// which is what the double really is.
///
/// `None` for a number no decimal can hold, which is a magnitude no double
/// reaches and so nothing a document can contain.
fn exact(n: &Number) -> Option<String> {
    let text = n.as_str();
    if text.contains(['e', 'E']) {
        crate::decimal::expanded(text)
    } else {
        Some(text.to_owned())
    }
}

/// A number written so that exactly the numbers [`compare`] calls equal share
/// a spelling.
///
/// By value, over the digits: `2.50`, `2.5` and `2.500` are one number and one
/// key, `1.0` meets `1`, `-0.0` meets `0`, and a big integer keys as itself
/// rather than as the f64 it does not fit in. The same [`crate::decimal`]
/// shortening a DECIMAL column keys under, so a document's number and a
/// column's group the same way.
fn number_key(n: &Number) -> String {
    if let Some(key) = exact(n).and_then(|text| crate::decimal::shortest(&text)) {
        return key;
    }
    format!("{:?}", n.as_f64().unwrap_or(f64::NAN))
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
        // As a JSON number at its own width, which serde_json's `u64` case
        // renders exactly. Through the `i64` reading it would have been -1:
        // `JSON_ARRAY(bu)` over 18446744073709551615 is `[18446744073709551615]`
        // on 8.3.
        Value::Unsigned(n) => Json::from(*n),
        // Spelled here rather than at the end, because a document holds every
        // number as the text it will be written as. `Number::from_f64` would
        // have stored serde's spelling, which is not MySQL's.
        Value::Float(f) => double_number(*f).map_or(Json::Null, Json::Number),
        // Widened, not through the six digits a FLOAT column displays:
        // MySQL's JSON number is a double, and `JSON_OBJECT('a', f)` over a
        // float holding 1.234567890123 gives `{"a": 1.2345678806304932}`.
        Value::Float32(f) => double_number(f64::from(*f)).map_or(Json::Null, Json::Number),
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
        // A number rather than a string — `JSON_OBJECT('a', decimal_column)`
        // gives `{"a": 2.50}` and not `{"a": "2.50"}` — and the *same* digits,
        // scale included. This is the one place the `arbitrary_precision`
        // feature is paying for itself: parsed into a `Number` there, the text
        // is what the number is, so the trailing zero the column's scale
        // exists to carry survives being embedded, stored, reloaded and
        // written back out.
        Value::Decimal(s) => s
            .parse::<Number>()
            .map_or_else(|_| Json::String(s.clone()), Json::Number),
        // Every route to a tagged value validates it first — a constructor
        // serialised it from a `Json`, a column coerced it, an image checked
        // it on load — so this parses what one of those wrote. The fallback is
        // unreachable rather than lenient; a JSON null beats re-encoding the
        // source text as a string, which is the double-encoding this removes.
        Value::Json(s) => parse_document(s).unwrap_or(Json::Null),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The signal [`Document`] reads the sentinel key by, pinned from both
    /// sides.
    ///
    /// It is a property of serde rather than of this code: the JSON reader
    /// hands a string to a visitor borrowed from the input, or from its
    /// unescaping scratch buffer, and hands a number it is holding as text
    /// over *owned*, because that comes out of the `String` the parser parked
    /// it in. Both directions are asserted, so a serde that stopped borrowing
    /// — or one that started handing numbers over borrowed — fails here
    /// rather than quietly turning one legal document into another.
    #[test]
    fn serde_still_tells_a_parked_number_from_a_member() {
        use serde::de::{Deserialize, IntoDeserializer};

        // The owned side, which is how a parked number arrives.
        let parked: serde::de::value::StringDeserializer<serde::de::value::Error> =
            String::from("2.50").into_deserializer();
        let owned = Sentinel::deserialize(parked).expect("a parked number deserialises");
        assert!(
            matches!(&owned, Sentinel::Digits(digits) if digits == "2.50"),
            "an owned string is serde_json's parked number"
        );

        // And the borrowed side, through the JSON reader itself, which is
        // what actually decides: the same digits written as a member's value
        // stay a member, and the number spelled the same way stays a number.
        assert_eq!(
            render_text(&read(r#"{"$serde_json::private::Number": "2.50"}"#).unwrap()),
            r#"{"$serde_json::private::Number": "2.50"}"#
        );
        assert_eq!(render_text(&read("2.50").unwrap()), "2.50");
    }

    /// Every shape of the sentinel key. Measured on 8.3, which stores it like
    /// any other key: all of these are objects, and the two-member ones have
    /// length two.
    #[test]
    fn the_sentinel_key_is_a_key() {
        for (text, expected) in [
            (
                r#"{"$serde_json::private::Number": "1"}"#,
                r#"{"$serde_json::private::Number": "1"}"#,
            ),
            (
                r#"{"$serde_json::private::Number": 1}"#,
                r#"{"$serde_json::private::Number": 1}"#,
            ),
            (
                r#"{"$serde_json::private::Number": "abc"}"#,
                r#"{"$serde_json::private::Number": "abc"}"#,
            ),
            // The value written with an escape, so it reaches the visitor
            // through serde's scratch buffer rather than borrowed. That is a
            // third delivery and it must land on the same side as borrowed.
            (
                r#"{"$serde_json::private::Number": "\u0031"}"#,
                r#"{"$serde_json::private::Number": "1"}"#,
            ),
            // And the *key* written with an escape, which decodes to the same
            // key and so cannot be caught by looking at the source text.
            (
                r#"{"\u0024serde_json::private::Number": "1"}"#,
                r#"{"$serde_json::private::Number": "1"}"#,
            ),
            // Beside another member, in both orders: only a first key can
            // ever be serde's, so the second order takes the other path.
            (
                r#"{"$serde_json::private::Number": "1", "b": 2}"#,
                r#"{"b": 2, "$serde_json::private::Number": "1"}"#,
            ),
            (
                r#"{"b": 2, "$serde_json::private::Number": "1"}"#,
                r#"{"b": 2, "$serde_json::private::Number": "1"}"#,
            ),
            // Nested beside a real number, which is the pair that has to come
            // out of one document as two different things.
            (
                r#"[2.50, {"$serde_json::private::Number": "1"}]"#,
                r#"[2.5, {"$serde_json::private::Number": "1"}]"#,
            ),
        ] {
            let parsed = parse_source(text).unwrap_or_else(|e| panic!("{text}\n  -> {e}"));
            assert_eq!(render_text(&parsed), expected, "{text}");
        }
    }
}
