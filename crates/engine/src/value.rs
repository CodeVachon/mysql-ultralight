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
    /// A fixed-point decimal, held as its text at the column's scale.
    ///
    /// Tagged for the same reason as `Json` above: nothing about `"2.50"` says
    /// whether it is a number or the characters someone stored in a VARCHAR,
    /// and the two are compared differently. A decimal compares by value, so
    /// `2.50` equals `2.5` and sorts below `10.00`; two strings compare by
    /// their characters, so `'2.5'` and `'2.50'` are *not* equal and `'10.00'`
    /// sorts first. Both of those are MySQL's answers, and only the tag can
    /// pick between them.
    Decimal(String),
    /// A four-byte float, held widened but remembered as four bytes.
    ///
    /// Tagged for the same reason `Decimal` above is: the value alone does not
    /// say how wide the column that produced it was, and MySQL shows a FLOAT
    /// at six significant digits where a DOUBLE gets as many as it needs. A
    /// column holding 1.234567890123 reads back `1.23457` there and would read
    /// back `1.2345678806304932` from a bare double — which is *further* from
    /// the truth than the unrounded value it replaced.
    ///
    /// It does not survive arithmetic, which is right: `f + 0` in MySQL is a
    /// double and shows every digit of the widened value.
    Float32(f32),
    /// A DATE, DATETIME, TIMESTAMP or TIME, held as its text.
    ///
    /// The sharpest case of the same argument `Json` and `Decimal` above make.
    /// MySQL reads a temporal in a numeric context as its packed digits — a
    /// DATE of 2024-01-05 is 20240105, a DATETIME 20240105060708, a TIME of
    /// 06:07:08 is 60708 — where a *string* of those same characters is read
    /// by its leading number and gives 2024 and 6. Both measured on 8.3.
    /// Nothing about `"2024-01-05"` says which was meant, so the tag is the
    /// only thing that can tell them apart.
    ///
    /// The tag rides on the value rather than being read off the expression
    /// because MySQL's own answer does: `DATE_ADD('2024-01-05', INTERVAL 1
    /// DAY)` is typed a *string* there — `STRING`, not `DATE` — and still
    /// reads 20240106 in arithmetic. A type taken from the statement would
    /// have said 2024.
    ///
    /// A YEAR is not one of these. MySQL reads it as the plain number it
    /// already is, and this engine stores it as `Int`.
    Temporal(String),
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
            // Rounded rather than truncated, which is measured: MySQL answers
            // `CAST(dt3 AS SIGNED)` 20240105060709 for a DATETIME(3) holding
            // 06:07:08.500, and 60709 for the TIME(3) beside it.
            Value::Temporal(_) => match self.temporal_number()? {
                Value::Int(n) => Some(n),
                Value::Decimal(text) => i64::try_from(crate::decimal::units(&text, 0)?).ok(),
                _ => None,
            },
            Value::Float(v) => Some(*v as i64),
            Value::Float32(v) => Some(*v as i64),
            Value::Text(v) | Value::Json(v) | Value::Decimal(v) => {
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
            Value::Float32(v) => Some(*v as f64),
            Value::Text(v) | Value::Json(v) | Value::Decimal(v) => v.trim().parse().ok(),
            // Its packed number, which is what makes `d = 20240105` true: a
            // comparison against a number is numeric, and reading the text
            // gave `None` here and sent the pair to the collation instead.
            Value::Temporal(_) => Some(self.as_number()),
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
            Value::Float32(v) => *v as f64,
            Value::Text(v) | Value::Json(v) | Value::Decimal(v) => numeric_prefix(v),
            Value::Bytes(v) => numeric_prefix(&String::from_utf8_lossy(v)),
            // Its packed digits, and its leading number only where the text is
            // not a temporal at all — a DATE column can hold text this engine
            // never validated, and that is no reason to answer differently
            // from the untagged value it used to be.
            Value::Temporal(v) => match self.temporal_number() {
                Some(number) => number.as_number(),
                None => numeric_prefix(v),
            },
        }
    }

    /// The value's string form, as MySQL would render it.
    pub fn as_text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format_float(*v),
            // Six significant digits, which is what MySQL shows for a FLOAT.
            Value::Float32(v) => mysql_ultralight_protocol::value::format_float(
                *v as f64,
                Some(mysql_ultralight_protocol::value::FLOAT_DIGITS),
            ),
            Value::Text(v) => v.clone(),
            Value::Bytes(v) => String::from_utf8_lossy(v).into_owned(),
            // The text it was stored as: only the *numeric* reading of a
            // temporal is packed, and `SELECT d` still shows `2024-01-05`.
            Value::Temporal(v) => v.clone(),
            // The document as it was serialised, which is what MySQL sends and
            // what a client reads back.
            Value::Json(v) | Value::Decimal(v) => v.clone(),
        }
    }

    /// SQL truthiness: non-zero is true, NULL is unknown.
    pub fn truthy(&self) -> Option<bool> {
        match self {
            Value::Null => None,
            Value::Int(v) => Some(*v != 0),
            Value::Float(v) => Some(*v != 0.0),
            Value::Float32(v) => Some(*v != 0.0),
            // A string is read for its leading number, so '0' and 'abc' are
            // both false and '1abc' is true. A binary string is read the same
            // way: treating it as "true when non-empty" made `WHERE BINARY x`
            // true for the byte '0', which is how a filter over a cast
            // comparison silently kept every row.
            Value::Text(v) => Some(numeric_prefix(v) != 0.0),
            Value::Bytes(v) => Some(numeric_prefix(&String::from_utf8_lossy(v)) != 0.0),
            // Read like the text it used to be, so tagging a value changed
            // nothing about how a WHERE clause treats it.
            Value::Json(v) | Value::Decimal(v) => Some(numeric_prefix(v) != 0.0),
            // By the packed number, for the same reason arithmetic is: MySQL
            // asks a temporal the numeric question the way it converts one.
            Value::Temporal(_) => Some(self.as_number() != 0.0),
        }
    }

    /// Three-valued comparison. `None` means the comparison involved NULL and
    /// is therefore unknown, which is not the same as "not equal".
    pub fn compare(&self, other: &Value) -> Option<std::cmp::Ordering> {
        if self.is_null() || other.is_null() {
            return None;
        }

        // A document compares as a document, not as its rendering. That is not
        // a refinement: comparing the text made `doc = '{"a": 1}'` true when
        // the literal happened to match MySQL's spacing, where MySQL reads the
        // literal as a JSON *string* and answers 0 because an object is never
        // a string. It also ordered by the alphabet instead of by JSON type.
        if matches!(self, Value::Json(_)) || matches!(other, Value::Json(_)) {
            return crate::json::compare(&self.as_json_operand(), &other.as_json_operand());
        }

        // Numbers compare numerically even against numeric strings, which is
        // how MySQL behaves and what Prisma's generated comparisons rely on.
        // A decimal is a number: `2.50` equals `2.5` and sorts below `10.00`,
        // where two *strings* of those characters do neither.
        let both_numeric = matches!(
            self,
            Value::Int(_) | Value::Float(_) | Value::Decimal(_) | Value::Float32(_)
        ) || matches!(
            other,
            Value::Int(_) | Value::Float(_) | Value::Decimal(_) | Value::Float32(_)
        );

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

    /// The exact number MySQL reads this temporal as, or `None` for anything
    /// that is not one.
    ///
    /// An integer where the value has no fractional seconds and a decimal
    /// where it has, because that is what MySQL types the arithmetic over one:
    /// `d + 0` is a LONGLONG and `dt3 + 0` a NEWDECIMAL that keeps all three
    /// digits — 20240105060708.500, not 20240105060708.5.
    ///
    /// `None` for a temporal whose text is not one, which a DATE column can
    /// hold because nothing validates on the way in. The callers fall back to
    /// the leading-number reading, which is what the untagged value gave.
    pub fn temporal_number(&self) -> Option<Value> {
        let Value::Temporal(text) = self else {
            return None;
        };
        let (negative, packed, fraction) = packed_temporal(text)?;
        let integer = packed.trim_start_matches('0');
        let integer = if integer.is_empty() { "0" } else { integer };
        let sign = if negative { "-" } else { "" };
        Some(if fraction.is_empty() {
            Value::Int(format!("{sign}{integer}").parse().ok()?)
        } else {
            Value::Decimal(format!("{sign}{integer}.{fraction}"))
        })
    }

    /// This value as the JSON operand MySQL would compare it as.
    ///
    /// A non-document beside a document becomes a JSON *scalar* of its own
    /// type — a string becomes a JSON string, which is why `doc = '{"a": 1}'`
    /// is false for every row: the literal is the string `{"a": 1}` and an
    /// object never equals a string.
    ///
    /// One case is out of reach. MySQL reads the SQL keyword `TRUE` as JSON
    /// `true` and the literal `1` as the number 1, and this engine evaluates
    /// both to `Value::Int(1)` — there is no boolean here to tell them apart.
    /// See #0061.
    fn as_json_operand(&self) -> serde_json::Value {
        crate::json::from_value(self)
    }

    /// The key this value sorts and de-duplicates under.
    ///
    /// Shares the collation with `compare`, so DISTINCT and uniqueness agree
    /// with what `WHERE` treats as equal — for two values of the same kind.
    /// Across the document boundary they do not: `compare` reads a string
    /// beside a document as a JSON string and can call the two equal, where
    /// the keys here stay a document key and a text key. Only a query that
    /// de-duplicates a JSON column against a text one can see it.
    pub fn collation_key(&self) -> String {
        match self {
            Value::Bytes(v) => String::from_utf8_lossy(v).into_owned(),
            // A document keeps its case and normalises its numbers, because
            // that is what comparing it does: `{"A":1}` and `{"a":1}` are two
            // documents where `'A'` and `'a'` are one string, and `1` and
            // `1.0` are one number where `'1'` and `'1.0'` are two strings.
            Value::Json(_) => crate::json::comparison_key(&self.as_json_operand()),
            // By value: every spelling of one number reaches one key, so
            // `2.50` and `2.5` are one group — and, because the key is the
            // number written back out, `2.50` is a *different* key from
            // `25.00`. Dropping the trailing zeros without the scale that went
            // with them made those two the same, and a GROUP BY over a money
            // column silently merged rows.
            //
            // The bare integer, too, so `2.00` keys the same as the `2` a
            // union might put beside it.
            Value::Decimal(text) => {
                let scale = crate::decimal::scale_of(text);
                match crate::decimal::units(text, scale) {
                    Some(units) => {
                        let (units, scale) = crate::decimal::shortest(units, scale);
                        crate::decimal::render(units, scale)
                    }
                    None => text.to_lowercase(),
                }
            }
            other => other.as_text().to_lowercase(),
        }
    }

    /// Converts to the wire representation.
    pub fn to_wire(&self) -> WireValue {
        match self {
            Value::Null => WireValue::Null,
            Value::Int(v) => WireValue::I64(*v),
            Value::Float(v) => WireValue::F64(*v),
            // Four bytes on the wire as well as in the column, which is the
            // other half of the same promise.
            Value::Float32(v) => WireValue::F32(*v),
            Value::Text(v) => WireValue::Str(v.clone()),
            Value::Bytes(v) => WireValue::Bytes(v.clone()),
            // JSON crosses the wire as text, which is how MySQL transmits it.
            // A decimal travels as its text, which is how MySQL sends one.
            // A temporal does too — the packed number is a reading, not a
            // representation, and a client asking for a DATE gets its text.
            Value::Json(v) | Value::Decimal(v) | Value::Temporal(v) => WireValue::Str(v.clone()),
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
    // One formatter for both crates: a value rendered here and the same value
    // rendered on its way out have to agree, and they did not — this dropped a
    // trailing `.0` and never reached for an exponent, so `1e20` was twenty-one
    // digits where MySQL writes `1e20`.
    mysql_ultralight_protocol::value::format_float(v, None)
}

/// The number MySQL reads out of the front of a string.
///
/// Not a parse of the whole thing: MySQL takes the leading numeric part and
/// ignores the rest, so `'12abc'` is 12 and `'abc'` is 0. Used wherever a
/// string has to answer a numeric question, which includes being true or false.
pub(crate) fn numeric_prefix(text: &str) -> f64 {
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

/// The digits MySQL packs a temporal into, as a sign, an integer part and a
/// fraction.
///
/// Positional rather than a parse: each group is widened to the place it
/// occupies, so `2024-1-5` and `2024-01-05` are one date and both pack as
/// 20240105. `None` where any group is not digits, which is the caller's
/// signal that the text is not a temporal at all.
fn packed_temporal(text: &str) -> Option<(bool, String, String)> {
    let trimmed = text.trim();
    let (body, fraction) = match trimmed.split_once('.') {
        Some((body, fraction)) if !fraction.is_empty() && all_digits(fraction) => (body, fraction),
        Some(_) => return None,
        None => (trimmed, ""),
    };
    // Only a TIME is ever negative, and MySQL keeps the sign on the packed
    // number: a TIME of -06:07:08 is -60708 rather than 60708.
    let negative = body.starts_with('-');
    let body = body.strip_prefix('-').unwrap_or(body);

    // A space or a `T` separates the calendar from the clock. Without one, a
    // colon anywhere says the whole value is a clock — that is the only thing
    // telling a bare TIME from a bare DATE.
    let (date, clock) = match body.split_once([' ', 'T']) {
        Some((date, clock)) => (Some(date), Some(clock.trim_start())),
        None if body.contains(':') => (None, Some(body)),
        None => (Some(body), None),
    };

    let mut packed = String::new();
    if let Some(date) = date {
        let mut groups = date.split('-');
        widen(groups.next()?, 4, &mut packed)?;
        for group in groups {
            widen(group, 2, &mut packed)?;
        }
    }
    if let Some(clock) = clock {
        let mut groups = 0;
        for group in clock.split(':') {
            // Widened, never truncated: a TIME runs to 838 hours, and
            // `838:59:59` packs as 8385959 rather than losing a digit.
            widen(group, 2, &mut packed)?;
            groups += 1;
        }
        // MySQL takes `2024-01-05 06:07` on the way in, and this engine stores
        // what was written. The missing groups pack as zero, which is what
        // MySQL reads that value as: 20240105060700.
        for _ in groups..3 {
            packed.push_str("00");
        }
    }
    Some((negative, packed, fraction.to_owned()))
}

/// Appends one group of a temporal, zero-padded to the width of its place.
fn widen(group: &str, width: usize, out: &mut String) -> Option<()> {
    if !all_digits(group) {
        return None;
    }
    for _ in group.len()..width {
        out.push('0');
    }
    out.push_str(group);
    Some(())
}

/// A non-empty run of ASCII digits and nothing else.
fn all_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
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

    /// The packer at its own level.
    ///
    /// Every temporal a *statement* can produce is normalised first — into its
    /// column's or its cast's declared shape — so the widening below is not
    /// reachable through SQL any more. It is still reachable through text
    /// nothing validated: `temporal::normalise` declines a date MySQL would
    /// have rejected on the way in, and this engine keeps such text rather
    /// than refusing the insert. So the rule is asserted here rather than
    /// through a query that would have to be contrived to reach it.
    #[test]
    fn a_temporal_packs_each_group_into_the_place_it_occupies() {
        let packed = |text: &str| Value::Temporal(text.to_owned()).temporal_number();

        assert_eq!(packed("2024-01-05"), Some(Value::Int(20_240_105)));
        assert_eq!(
            packed("2024-01-05 06:07:08"),
            Some(Value::Int(20_240_105_060_708))
        );
        assert_eq!(packed("06:07:08"), Some(Value::Int(60_708)));
        // Groups narrower than their place are widened, not concatenated:
        // `2024-1-5` is the fifth of January and not 2024105.
        assert_eq!(packed("2024-1-5"), Some(Value::Int(20_240_105)));
        assert_eq!(
            packed("2024-1-5 6:7:8"),
            Some(Value::Int(20_240_105_060_708))
        );
        // A clock with groups missing packs the rest as zero rather than
        // shortening the number.
        assert_eq!(
            packed("2024-01-05 06:07"),
            Some(Value::Int(20_240_105_060_700))
        );
        assert_eq!(
            packed("2024-01-05 06"),
            Some(Value::Int(20_240_105_060_000))
        );
        // A TIME keeps its sign and runs past two digits of hours.
        assert_eq!(packed("-06:07:08"), Some(Value::Int(-60_708)));
        assert_eq!(packed("838:59:59"), Some(Value::Int(8_385_959)));
        // A fraction packs after the point, keeping exactly the digits given.
        assert_eq!(
            packed("2024-01-05 06:07:08.500"),
            Some(Value::Decimal("20240105060708.500".to_owned()))
        );
        assert_eq!(
            packed("06:07:08.5"),
            Some(Value::Decimal("60708.5".to_owned()))
        );
        // And text that is not a temporal at all has no packed number, which
        // is how the callers know to read it as a string instead.
        assert_eq!(packed("hello"), None);
        assert_eq!(packed("2024-01-05 x"), None);
        assert_eq!(packed("06:07:08.5x"), None);
    }

    #[test]
    fn floats_render_without_a_trailing_zero() {
        assert_eq!(Value::Float(2.0).as_text(), "2");
        assert_eq!(Value::Float(2.5).as_text(), "2.5");
    }
}
