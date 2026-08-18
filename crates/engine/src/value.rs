//! Stored values and the comparison rules over them.

use std::cmp::Ordering;

use mysql_ultralight_protocol::Value as WireValue;

/// A value as the engine holds it.
///
/// Narrower than the wire type on purpose: MySQL transmits decimals, dates and
/// JSON as text, so storing them as text keeps one representation rather than
/// converting at both ends.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn text(s: impl Into<String>) -> Self {
        Value::Text(s.into())
    }

    /// The value as an integer, where that is meaningful.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            Value::Float(v) => Some(*v as i64),
            Value::Text(v) => {
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
            Value::Text(v) => v.trim().parse().ok(),
            _ => None,
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
        }
    }

    /// SQL truthiness: non-zero is true, NULL is unknown.
    pub fn truthy(&self) -> Option<bool> {
        match self {
            Value::Null => None,
            Value::Int(v) => Some(*v != 0),
            Value::Float(v) => Some(*v != 0.0),
            Value::Text(v) => Some(v.trim().parse::<f64>().map(|f| f != 0.0).unwrap_or(false)),
            Value::Bytes(v) => Some(!v.is_empty()),
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
