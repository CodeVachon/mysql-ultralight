//! Stored values and the comparison rules over them.

use std::cmp::Ordering;

use mysql_ultralight_protocol::Value as WireValue;

/// A value as the engine holds it.
///
/// Narrower than the wire type on purpose: MySQL transmits decimals and dates
/// as text, so storing them as text keeps one representation rather than
/// converting at both ends.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// A JSON document, held as its serialised text.
    ///
    /// Text as well, then — but distinguishable from it, which is the whole
    /// point. Without the tag, the result of one JSON function reaching
    /// another is an ordinary string, and `JSON_OBJECT('a', JSON_ARRAY(1, 2))`
    /// embeds it as one: `{"a": "[1,2]"}` where MySQL gives `{"a": [1, 2]}`.
    /// Nothing about the string itself says which was meant, so the tag is the
    /// only thing that can tell them apart.
    Json(String),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn text(s: impl Into<String>) -> Self {
        Value::Text(s.into())
    }

    /// The value as an integer, where that is meaningful.
    ///
    /// A document reads like the text it is: a JSON column holding `3` is 3 in
    /// arithmetic, as it was before documents were tagged, and refusing here
    /// would turn `doc + 0` into NULL for a value MySQL happily adds.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            Value::Float(v) => Some(*v as i64),
            Value::Text(v) | Value::Json(v) => {
                let t = v.trim();
                t.parse::<i64>()
                    .ok()
                    .or_else(|| t.parse::<f64>().ok().map(|f| f as i64))
            }
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Int(v) => Some(*v as f64),
            Value::Float(v) => Some(*v),
            Value::Text(v) | Value::Json(v) => v.trim().parse().ok(),
            _ => None,
        }
    }

    /// The number MySQL reads out of any non-NULL value.
    ///
    /// Every type answers, because MySQL's arithmetic has no "this is not a
    /// number" case: `'x' + 1` is 1 and `'x' * 2` is 0, where returning NULL
    /// for anything unparseable made both of those NULL. NULL itself is
    /// handled before an operand reaches this.
    pub fn as_number(&self) -> f64 {
        match self {
            Value::Null => 0.0,
            Value::Int(v) => *v as f64,
            Value::Float(v) => *v,
            Value::Text(v) | Value::Json(v) => numeric_prefix(v),
            Value::Bytes(v) => numeric_prefix(&String::from_utf8_lossy(v)),
        }
    }

    /// The value's string form, as MySQL would render it.
    pub fn as_text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format_float(*v),
            Value::Text(v) => v.clone(),
            Value::Bytes(v) => String::from_utf8_lossy(v).into_owned(),
            // The document as it was serialised, which is what MySQL sends and
            // what a client reads back.
            Value::Json(v) => v.clone(),
        }
    }

    /// SQL truthiness: non-zero is true, NULL is unknown.
    pub fn truthy(&self) -> Option<bool> {
        match self {
            Value::Null => None,
            Value::Int(v) => Some(*v != 0),
            Value::Float(v) => Some(*v != 0.0),
            // A string is read for its leading number, so '0' and 'abc' are
            // both false and '1abc' is true. A binary string is read the same
            // way: treating it as "true when non-empty" made `WHERE BINARY x`
            // true for the byte '0', which is how a filter over a cast
            // comparison silently kept every row.
            Value::Text(v) => Some(numeric_prefix(v) != 0.0),
            Value::Bytes(v) => Some(numeric_prefix(&String::from_utf8_lossy(v)) != 0.0),
            // Read like the text it used to be, so tagging a value changed
            // nothing about how a WHERE clause treats it.
            Value::Json(v) => Some(numeric_prefix(v) != 0.0),
        }
    }

    /// Three-valued comparison. `None` means the comparison involved NULL and
    /// is therefore unknown, which is not the same as "not equal".
    pub fn compare(&self, other: &Value) -> Option<std::cmp::Ordering> {
        if self.is_null() || other.is_null() {
            return None;
        }

        // Numbers compare numerically even against numeric strings, which is
        // how MySQL behaves and what Prisma's generated comparisons rely on.
        let both_numeric = matches!(self, Value::Int(_) | Value::Float(_))
            || matches!(other, Value::Int(_) | Value::Float(_));

        if both_numeric {
            if let (Some(a), Some(b)) = (self.as_float(), other.as_float()) {
                return a.partial_cmp(&b);
            }
        }

        if let (Value::Int(a), Value::Int(b)) = (self, other) {
            return Some(a.cmp(b));
        }

        match (self, other) {
            // Binary data is compared byte-exact; only text carries a collation.
            (Value::Bytes(a), Value::Bytes(b)) => Some(a.cmp(b)),
            _ => Some(collate(&self.as_text(), &other.as_text())),
        }
    }

    /// The key this value sorts and de-duplicates under.
    ///
    /// Shares the collation with `compare`, so DISTINCT and uniqueness agree
    /// with what `WHERE` treats as equal.
    pub fn collation_key(&self) -> String {
        match self {
            Value::Bytes(v) => String::from_utf8_lossy(v).into_owned(),
            other => other.as_text().to_lowercase(),
        }
    }

    /// Converts to the wire representation.
    pub fn to_wire(&self) -> WireValue {
        match self {
            Value::Null => WireValue::Null,
            Value::Int(v) => WireValue::I64(*v),
            Value::Float(v) => WireValue::F64(*v),
            Value::Text(v) => WireValue::Str(v.clone()),
            Value::Bytes(v) => WireValue::Bytes(v.clone()),
            // JSON crosses the wire as text, which is how MySQL transmits it.
            Value::Json(v) => WireValue::Str(v.clone()),
        }
    }

    /// Converts a bound parameter from the wire.
    pub fn from_wire(value: &WireValue) -> Self {
        match value {
            WireValue::Null => Value::Null,
            WireValue::I64(v) => Value::Int(*v),
            WireValue::U64(v) => Value::Int(*v as i64),
            WireValue::F32(v) => Value::Float(*v as f64),
            WireValue::F64(v) => Value::Float(*v),
            WireValue::Str(v) => Value::Text(v.clone()),
            WireValue::Bytes(v) => Value::Bytes(v.clone()),
        }
    }
}

/// Compares two strings under the server's advertised collation.
///
/// `utf8mb4_0900_ai_ci` is case-insensitive, which is MySQL's default and what
/// clients expect: `WHERE email = 'Foo@Bar.com'` must find `foo@bar.com`, and a
/// unique index must reject that pair. The accent-insensitive half of the
/// collation is not implemented.
fn collate(a: &str, b: &str) -> Ordering {
    let mut left = a.chars().flat_map(char::to_lowercase);
    let mut right = b.chars().flat_map(char::to_lowercase);

    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => continue,
                ord => return ord,
            },
        }
    }
}

/// Renders a float the way MySQL does: no trailing `.0`, no exponent for
/// ordinary magnitudes.
fn format_float(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v < 0.0 {
            "-Inf".to_owned()
        } else {
            "Inf".to_owned()
        };
    }
    let s = v.to_string();
    s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
}

/// The number MySQL reads out of the front of a string.
///
/// Not a parse of the whole thing: MySQL takes the leading numeric part and
/// ignores the rest, so `'12abc'` is 12 and `'abc'` is 0. Used wherever a
/// string has to answer a numeric question, which includes being true or false.
fn numeric_prefix(text: &str) -> f64 {
    let trimmed = text.trim_start();
    let bytes = trimmed.as_bytes();
    let digit = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    let sign = |i: usize| matches!(bytes.get(i), Some(b'-' | b'+'));

    let mut end = usize::from(sign(0));
    let mut digits = 0;
    while digit(end) {
        end += 1;
        digits += 1;
    }
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        while digit(end) {
            end += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return 0.0;
    }

    // An exponent counts only when it is complete, so `'1e2'` is 100 and `'1e'`
    // is 1 — reading the `e` and stopping there made `'1e2' + 1` answer 2.
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut after = end + 1 + usize::from(sign(end + 1));
        if digit(after) {
            while digit(after) {
                after += 1;
            }
            end = after;
        }
    }
    // Clamped rather than left infinite: MySQL stops a string conversion at
    // DBL_MAX, and an infinity renders as the token `Inf`, which is not a
    // number any client can read back.
    match trimmed[..end].parse().unwrap_or(0.0) {
        v if v == f64::INFINITY => f64::MAX,
        v if v == f64::NEG_INFINITY => f64::MIN,
        v => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn null_comparisons_are_unknown_not_unequal() {
        assert_eq!(Value::Null.compare(&Value::Int(1)), None);
        assert_eq!(Value::Int(1).compare(&Value::Null), None);
        assert_eq!(Value::Null.compare(&Value::Null), None);
    }

    #[test]
    fn numbers_and_numeric_strings_compare_numerically() {
        assert_eq!(
            Value::Int(10).compare(&Value::text("9")),
            Some(Ordering::Greater)
        );
        // Lexicographically "10" < "9", so this is the case that catches a
        // comparison done on text by mistake.
        assert_eq!(
            Value::text("10").compare(&Value::Int(9)),
            Some(Ordering::Greater)
        );
    }

    /// The server reports a case-insensitive collation, so comparison must be
    /// case-insensitive too or the metadata misleads the client.
    #[test]
    fn text_comparison_is_case_insensitive() {
        assert_eq!(
            Value::text("Foo@Bar.com").compare(&Value::text("foo@bar.com")),
            Some(Ordering::Equal)
        );
        assert_eq!(
            Value::text("ABC").compare(&Value::text("abd")),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn the_collation_key_agrees_with_comparison() {
        assert_eq!(
            Value::text("Foo").collation_key(),
            Value::text("foo").collation_key()
        );
    }

    #[test]
    fn text_compares_lexicographically() {
        assert_eq!(
            Value::text("abc").compare(&Value::text("abd")),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn truthiness_follows_mysql() {
        assert_eq!(Value::Int(0).truthy(), Some(false));
        assert_eq!(Value::Int(-1).truthy(), Some(true));
        assert_eq!(Value::Null.truthy(), None);
        assert_eq!(Value::text("abc").truthy(), Some(false));
        assert_eq!(Value::text("1").truthy(), Some(true));
    }

    #[test]
    fn floats_render_without_a_trailing_zero() {
        assert_eq!(Value::Float(2.0).as_text(), "2");
        assert_eq!(Value::Float(2.5).as_text(), "2.5");
    }
}
