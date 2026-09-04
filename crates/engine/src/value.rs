//! Stored values and the comparison rules over them.

use std::cmp::Ordering;
use std::fmt::Write as _;

use mysql_ultralight_protocol::Value as WireValue;

use crate::collation::{Collated, Collation};

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
    /// Text, and the collation it compares under.
    ///
    /// The tag rides on the value for the same reason `Temporal` and `Enum`
    /// below carry theirs: the value is the only thing that reaches the
    /// comparison. By the time `WHERE name = 'cafe'` is deciding a row, the
    /// column that said `COLLATE utf8mb4_unicode_ci` is out of reach, and
    /// without the tag every string in the engine compared under one rule —
    /// which is #0129, #0134 and #0152 together.
    ///
    /// It carries *how firmly* the collation is held as well as which one,
    /// because MySQL resolves a comparison from both sides and a literal loses
    /// to a column. See [`crate::collation::resolve`].
    ///
    /// [`Value::text`] builds the coercible kind, which is what a literal and
    /// anything computed from one carries;
    /// [`crate::types::SqlType::coerce`] is where a column's own collation
    /// goes on, since that is the last place the declaration and the value are
    /// both in hand.
    Text(String, Collated),
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
    /// A fixed-point decimal: its exact digits, and the scale it is *shown*
    /// at where that is narrower.
    ///
    /// Tagged for the same reason as `Json` above: nothing about `"2.50"` says
    /// whether it is a number or the characters someone stored in a VARCHAR,
    /// and the two are compared differently. A decimal compares by value, so
    /// `2.50` equals `2.5` and sorts below `10.00`; two strings compare by
    /// their characters, so `'2.5'` and `'2.50'` are *not* equal and `'10.00'`
    /// sorts first. Both of those are MySQL's answers, and only the tag can
    /// pick between them.
    ///
    /// The second field is why a quotient is right in both readings. MySQL's
    /// division computes to a nine-digit word boundary and declares the column
    /// at the left operand's scale plus `div_precision_increment`, so the
    /// value and its rendering are different numbers: measured on 8.3,
    /// `SELECT 1/3` prints 0.3333 while `JSON_ARRAY(1/3)` is `[0.333333333]`,
    /// `1/3 = 0.3333` is 0 and `(1/3)*3` is 1.0000 rather than 0.9999. So the
    /// text here is the *value*, always, and `Some(n)` says the client sees it
    /// at `n` places instead. `None` is the ordinary case — a literal, a
    /// column, anything already at its own scale — and means the two agree.
    ///
    /// Everything that reads a decimal as a number reads the text: comparison,
    /// arithmetic, `SUM`, a document, and a column it is written into, all
    /// measured. Everything that reads it as characters reads the rendering:
    /// the wire, `CAST(1/3 AS CHAR)`, `CONCAT` and `GROUP_CONCAT`. See #0118
    /// and #0122.
    ///
    /// [`Value::decimal`] builds the agreeing kind and [`Value::decimal_at`]
    /// the split one; [`Value::displayed`] is the rendering and
    /// [`Value::settled`] collapses the two, which is what a temporary table
    /// does — `SELECT JSON_ARRAY(x) FROM (SELECT 1/3 AS x) t` is `[0.3333]` on
    /// 8.3 where the same expression unmaterialised is `[0.333333333]`.
    ///
    /// A byte, not a `usize`, so that carrying the second field costs nothing:
    /// `Value` is passed by value through the whole evaluator and a recursive
    /// one is measured in stack frames. MySQL's widest scale anywhere is the
    /// 81 places a quotient's value reaches, which is well inside a `u8`.
    Decimal(String, Option<u8>),
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
    /// An ENUM member, with the 1-based position it was declared at.
    ///
    /// The same argument `Temporal` above makes, over a different number. An
    /// ENUM *is* its ordinal in every numeric context — `ENUM('a','b')`
    /// holding `'a'` reads 1, so `e + 0` is 1, `-e` is -1 and `e = 1` is true
    /// — and it is its member everywhere else, so `SELECT e` shows `a` and
    /// `e = 'a'` is true. Stored as text alone, the number had to be read out
    /// of the characters, which answers 0 for every member that is not itself
    /// a number.
    ///
    /// The ordinal is carried rather than looked up because the value is the
    /// only thing that reaches the evaluator: `SqlType::variants` has the
    /// declaration order, and by the time an operand is being added to
    /// something its column is out of reach. It also settles the case that
    /// proves the ordinal is not the text — `ENUM('7','8')` holding `'7'`
    /// reads 1, not 7, measured on 8.3.
    ///
    /// Ordinal 0 with an empty member is MySQL's error value, which a column
    /// holds after an assignment that matched no member. It is a legal state,
    /// not a corrupt one: `g = '0'` stores it and it reads back as `''` and 0.
    Enum {
        member: String,
        ordinal: u32,
    },
    /// A SQL boolean: `TRUE`, `FALSE`, or the answer of anything that asks a
    /// yes-or-no question.
    ///
    /// The same argument the tags above make, over the one place MySQL can
    /// still tell 1 from true: JSON. `CAST('true' AS JSON) = TRUE` is 1 there
    /// and `= 1` is 0, because the keyword converts to the JSON *boolean* and
    /// the literal to the JSON *number*. Read as `Int(1)`, both operands
    /// became the number and the first row answered 0.
    ///
    /// Everywhere else it reads exactly as `Int(0|1)` does — `TRUE = 1` is 1,
    /// `SELECT TRUE` sends `1`, `SUM(TRUE)` is 1 — so the tag changes the
    /// *document* reading of a boolean and nothing else.
    ///
    /// It is not only the keyword. Measured on 8.3, `JSON_ARRAY(1 = 1)`,
    /// `JSON_ARRAY(NOT 0)`, `JSON_ARRAY(1 AND 1)`, `JSON_ARRAY(1 IS TRUE)`
    /// and `JSON_ARRAY(1 IN (1, 2))` are all `[true]`, so every comparison,
    /// logical operator and postfix test carries it. That is why it is built
    /// in [`crate::expr::bool_value`], the one funnel all of those already
    /// went through, rather than at the literal alone.
    ///
    /// No column can hold one, which is measured rather than assumed: a
    /// `BOOLEAN` column is a `TINYINT(1)`, and `JSON_ARRAY(b)` over one
    /// holding 1 is `[1]` while `j = b` against a document of `true` is 0.
    /// So [`crate::types::SqlType::coerce`] — the only way a value enters a
    /// row — strips it, and the state image never has to know the tag exists.
    Bool(bool),
    /// An integer MySQL is treating as UNSIGNED.
    ///
    /// Two things at once, and both are needed. It is a *magnitude* an `i64`
    /// cannot hold — `BIGINT UNSIGNED` reaches 18446744073709551615 and every
    /// bitwise operator answers in that range, so `SELECT ~0` had no
    /// representation and was refused — and it is the *tag* that decides how
    /// an operator behaves: measured on 8.3, `iu - 100` over an `INT UNSIGNED`
    /// holding 7 raises 1690 rather than answering -93, and it does so because
    /// the subtraction is unsigned, not because 7 is small.
    ///
    /// The range check is per operator, which is measured and is the opposite
    /// of what #0052 concluded for the bitwise chain: `iu - 100 + 200` raises
    /// 1690 naming `(iu - 100)` even though 107 would fit. So the answer is
    /// not "check what the chain hands out" — it is "compute exactly, and
    /// check against the domain this operator works in". See
    /// [`crate::expr::arithmetic`].
    ///
    /// The sign is a property of the *column*, so it is applied in
    /// [`crate::types::SqlType::coerce`] where the declaration is still in
    /// hand — the same argument `Enum` above makes about its ordinal. A
    /// literal above `i64::MAX` carries it too, because MySQL types one
    /// `BIGINT UNSIGNED`: `SELECT 18446744073709551615 - 1` answers there.
    ///
    /// It comes back off wherever MySQL's own result type is signed — unary
    /// minus, a conditional with a signed candidate, a set operation whose
    /// branches disagree — see [`Value::untag_unsigned`].
    Unsigned(u64),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Text with no collation of its own: a literal, or anything built from
    /// one. It takes the collation of whatever it is compared against.
    pub fn text(s: impl Into<String>) -> Self {
        Value::Text(s.into(), Collated::COERCIBLE)
    }

    /// Text carrying a column's declared collation.
    pub fn collated(s: impl Into<String>, collation: Collation) -> Self {
        Value::Text(s.into(), Collated::implicit(collation))
    }

    /// A decimal shown at exactly the places it carries: a literal, a column,
    /// anything whose value and rendering are one number.
    pub fn decimal(text: impl Into<String>) -> Self {
        Value::Decimal(text.into(), None)
    }

    /// A decimal whose value carries more places than the client is shown.
    ///
    /// A quotient, and everything computed from one. See the note on
    /// [`Value::Decimal`].
    pub fn decimal_at(text: impl Into<String>, display: usize) -> Self {
        let text = text.into();
        // Collapsed where the two scales are the same number, which is most of
        // what comes through here — a sum, a difference and a remainder are
        // shown at exactly the places they carry unless a quotient was one of
        // the operands. It saves the rescale on every read, and it is what
        // keeps the byte below from having to hold a scale no clamp bounds: a
        // *literal* can be written to more places than a `u8` counts, and the
        // paths that widen past 30 — `+`, `-` and `%` — are the ones this arm
        // takes.
        if display == crate::decimal::scale_of(&text) {
            return Value::Decimal(text, None);
        }
        // Saturating rather than wrapping, for the value that would reach here
        // some other way: showing every place it carries beats showing a
        // number of them nobody asked for.
        Value::Decimal(text, Some(display.min(u8::MAX as usize) as u8))
    }

    /// How many places this value is *shown* at, for an operand of exact
    /// arithmetic.
    ///
    /// An integer shows none; a decimal shows its declared scale, which is its
    /// own where it has no narrower one. `None` for everything else, which is
    /// the answer the exact paths already fall out of.
    pub(crate) fn display_scale(&self) -> Option<usize> {
        match self {
            Value::Int(_) | Value::Unsigned(_) => Some(0),
            Value::Decimal(text, display) => {
                Some(display.map_or_else(|| crate::decimal::scale_of(text), usize::from))
            }
            _ => None,
        }
    }

    /// The characters a client sees, for a decimal that shows fewer places
    /// than it carries.
    ///
    /// Borrowed where the two agree, which is every value that is not a
    /// quotient or built from one.
    ///
    /// Anything that is not a decimal is handed to [`Value::as_text`], which
    /// is not a cycle: the only arm of that function reaching back here is the
    /// decimal one, which the two arms above have already taken.
    pub(crate) fn displayed(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Value::Decimal(text, Some(display)) => {
                match crate::decimal::rescale_wide(text, usize::from(*display)) {
                    Some(shown) => std::borrow::Cow::Owned(shown),
                    // Nothing builds a `Decimal` out of anything but digits, so
                    // this is unreachable; answering the text keeps a value that
                    // got here another way readable rather than empty.
                    None => std::borrow::Cow::Borrowed(text.as_str()),
                }
            }
            Value::Decimal(text, None) => std::borrow::Cow::Borrowed(text.as_str()),
            _ => std::borrow::Cow::Owned(self.as_text()),
        }
    }

    /// The value a temporary table would hold: a decimal cut to the scale it
    /// is shown at, everything else unchanged.
    ///
    /// MySQL's derived tables, CTEs and set operations all write their rows
    /// into a table whose field carries the *declared* scale, so the extra
    /// places a quotient computed do not survive one. Measured on 8.3:
    /// `SELECT JSON_ARRAY(x) FROM (SELECT 1/3 AS x) t` is `[0.3333]` where the
    /// bare `SELECT JSON_ARRAY(1/3)` is `[0.333333333]`, and a scalar
    /// subquery — which is not materialised — keeps the nine.
    pub(crate) fn settled(self) -> Self {
        match self {
            Value::Decimal(text, Some(display)) => Value::decimal(
                crate::decimal::rescale_wide(&text, usize::from(display)).unwrap_or(text),
            ),
            other => other,
        }
    }

    /// The collation this value brings to a comparison.
    ///
    /// Everything that is not text is NUMERIC: MySQL's coercibility 5, weaker
    /// than a literal's 4, which is what makes a literal win against it rather
    /// than tie. It used to be COERCIBLE here, on the grounds that two of
    /// these never meet as strings anyway — true, and beside the point once a
    /// literal started carrying the *connection's* collation instead of the
    /// server's. An ENUM member compared with a literal was then two COERCIBLE
    /// operands with two different collations, which [`crate::collation::resolve`]
    /// refuses with 1267: `SELECT (SELECT e FROM t) = 'b'` raised where 8.3
    /// answers a row. See [`crate::collation::Coercibility::Numeric`].
    pub(crate) fn collated_as(&self) -> Collated {
        match self {
            Value::Text(_, collated) => *collated,
            // Bytes compare exactly, and they say so at IMPLICIT strength so
            // one binary operand decides the pair — MySQL's own rule, and the
            // one `compare` below already applies by sending any pairing with
            // a `Bytes` in it to a byte comparison. Saying it here as well is
            // what makes `LIKE` agree with `=` about them: measured on 8.3,
            // `CAST('A' AS BINARY) LIKE 'a'` is 0, and without this the
            // pattern's coercible `utf8mb4_0900_ai_ci` decided the match and
            // folded the case. `utf8mb4_bin` rather than the `binary`
            // character set, because the two ask for the same comparison and
            // only one of them is a comparison this engine has.
            Value::Bytes(_) => Collated::implicit(Collation::Bin),
            _ => Collated::NUMERIC,
        }
    }

    /// This value with the connection's collation on it, where it holds none
    /// of its own.
    ///
    /// A literal's collation is `collation_connection`, not the server's
    /// default: measured on 8.3 either side of one `SET NAMES`,
    /// `COLLATION('x')` follows the connection while `COERCIBILITY('x')` stays
    /// 4. So only COERCIBLE is touched — a column keeps the IMPLICIT collation
    /// it was declared with and a `COLLATE` clause keeps its EXPLICIT one, and
    /// both go on winning the resolution against a literal.
    ///
    /// Applied where a string is *built* — a literal, a function's answer, a
    /// cast, an aggregate — rather than at every site that builds a
    /// `Value::Text`. See #0154.
    pub(crate) fn on_connection(&mut self, collation: Collation) {
        if let Value::Text(_, collated) = self {
            if collated.coercibility == crate::collation::Coercibility::Coercible {
                collated.collation = collation;
            }
        }
    }

    /// This value as a user variable holds it, which is at IMPLICIT strength.
    ///
    /// A user variable keeps the collation of whatever was assigned to it and
    /// holds it firmly: measured on 8.3, `COERCIBILITY(@x)` is 2 and
    /// `COLLATION(@x)` is the connection's collation *at assignment*, unchanged
    /// by a later `SET NAMES`. So a literal loses to it, and `SET @x = 'a '`
    /// under `utf8mb4_0900_ai_ci` keeps `@x = 'a'` at 0 however the connection
    /// moves afterwards.
    ///
    /// Only text carries a collation, so everything else passes through.
    pub(crate) fn assigned(self) -> Value {
        match self {
            Value::Text(text, collated) => {
                Value::Text(text, Collated::implicit(collated.collation))
            }
            other => other,
        }
    }

    /// This value with `collation` named explicitly, as `x COLLATE c` does.
    ///
    /// Only text carries one. `1 COLLATE utf8mb4_bin` is 1253 on 8.3 —
    /// "COLLATION 'utf8mb4_bin' is not valid for CHARACTER SET 'binary'" — and
    /// that refusal is raised where the clause is read, so anything reaching
    /// here is already text.
    pub(crate) fn with_collation(self, collation: Collation) -> Value {
        match self {
            Value::Text(text, _) => Value::Text(text, Collated::explicit(collation)),
            other => other,
        }
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
                Value::Decimal(text, _) => i64::try_from(crate::decimal::units(&text, 0)?).ok(),
                _ => None,
            },
            // Its ordinal, which is the whole point of the tag: a member
            // that is not itself a number read 0 out of its own characters.
            Value::Enum { ordinal, .. } => Some(i64::from(*ordinal)),
            Value::Bool(b) => Some(i64::from(*b)),
            // Wrapped rather than refused, which is MySQL's own answer:
            // `CAST(bu AS SIGNED)` over a BIGINT UNSIGNED holding
            // 18446744073709551615 is -1 on 8.3, not an error.
            Value::Unsigned(v) => Some(*v as i64),
            Value::Float(v) => Some(*v as i64),
            Value::Float32(v) => Some(*v as i64),
            // Through the scalar the document holds rather than through its
            // rendering, so a quoted numeric is unquoted on the way — #0084.
            Value::Json(v) => digits_as_int(&crate::json::numeric_text(v)),
            Value::Text(v, _) | Value::Decimal(v, _) => digits_as_int(v),
            // Read the way the text arm above is, for the reason `as_float`
            // gives: a binary string is a string, and only its collation is
            // different. Left out, the two readings disagreed — a value that
            // an `INT` column refused with 1366 went into the `DOUBLE` column
            // beside it as a number, where 8.3 takes it into both.
            Value::Bytes(v) => digits_as_int(std::str::from_utf8(v).ok()?),
            _ => None,
        }
    }

    /// The value as an exact integer, wide enough for either sign's range.
    ///
    /// `as_int` above cannot answer this question: it returns an `i64`, so
    /// every value above `i64::MAX` comes back saturated or wrapped, and a
    /// `BIGINT UNSIGNED` column checking its own range against that would
    /// refuse the top half of what it can hold. `i128` covers both ranges
    /// exactly and nothing else about the reading changes.
    pub fn as_exact_int(&self) -> Option<i128> {
        match self {
            Value::Unsigned(v) => Some(i128::from(*v)),
            // Parsed at the full width before falling back to a float, so
            // `'18446744073709551615'` into a `BIGINT UNSIGNED` is the number
            // it spells and not the nearest double to it.
            Value::Text(v, _) | Value::Decimal(v, _) => digits_as_exact_int(v),
            // The scalar, not the rendering — #0084. At the full width for the
            // same reason the text arm is: 8.3 takes the JSON string
            // `"18446744073709551615"` into a `BIGINT UNSIGNED` as the number
            // it spells.
            Value::Json(v) => digits_as_exact_int(&crate::json::numeric_text(v)),
            // At the full width here as well, and not through `as_int` below.
            // That returns an `i64`, so a binary string spelling
            // 18446744073709551615 came back saturated at `i64::MAX` — and
            // this is the reading a `BIGINT UNSIGNED` column range-checks
            // itself with, so the column *stored* 9223372036854775807 where
            // the same digits as text stored the number they spell. Refusing
            // the value would have been wrong too; 8.3 takes it. A clear
            // error turning into a wrong answer is the one trade this repo
            // does not make.
            Value::Bytes(digits) => digits_as_exact_int(std::str::from_utf8(digits).ok()?),
            Value::Float(v) => Some(*v as i128),
            Value::Float32(v) => Some(*v as i128),
            other => other.as_int().map(i128::from),
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Int(v) => Some(*v as f64),
            Value::Float(v) => Some(*v),
            Value::Float32(v) => Some(*v as f64),
            Value::Text(v, _) | Value::Decimal(v, _) => v.trim().parse().ok(),
            // The scalar, not the rendering — #0084.
            Value::Json(v) => crate::json::numeric_text(v).trim().parse().ok(),
            // A binary string is still a *string*: MySQL reads a `BINARY`
            // value, a `VARBINARY` column and a `BLOB` in a numeric context
            // exactly as it reads text, and only the collation differs.
            // Answering `None` here sent `CAST('12' AS BINARY) > 3` to the
            // string comparison below, which decided it by the character `1`
            // against `3` and answered 0 where 8.3 answers 1;
            // `CAST('1' AS BINARY) = 1.0` was 0 for the same reason. Bytes
            // that are not UTF-8 could not spell a number anyway. See #0139.
            //
            // A *hex literal* is the exception, and this does not fix it: 8.3
            // reads `X'0F'` in a numeric context as the number 15, not as the
            // characters its byte spells, so `X'0F' = 15` is 1 there and 0
            // here. One `Value::Bytes` and two readings — the same split
            // `bit_operand` in `expr.rs` documents. That is #0141.
            Value::Bytes(v) => std::str::from_utf8(v).ok()?.trim().parse().ok(),
            // Its ordinal, which is what makes `e = 1` true: a comparison
            // against a number is numeric, and reading the member's text gave
            // `None` here and sent the pair to the collation instead.
            Value::Enum { ordinal, .. } => Some(f64::from(*ordinal)),
            Value::Bool(b) => Some(f64::from(u8::from(*b))),
            Value::Unsigned(v) => Some(*v as f64),
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
            Value::Text(v, _) | Value::Decimal(v, _) => numeric_prefix(v),
            // The scalar, not the rendering — #0084. `"2026" + 0` is 2026 on
            // 8.3 and was 0 here, because the leading quote stops
            // `numeric_prefix` before it reaches a digit.
            Value::Json(v) => numeric_prefix(&crate::json::numeric_text(v)),
            Value::Bytes(v) => numeric_prefix(&String::from_utf8_lossy(v)),
            Value::Enum { ordinal, .. } => f64::from(*ordinal),
            Value::Bool(b) => f64::from(u8::from(*b)),
            Value::Unsigned(v) => *v as f64,
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

    /// The characters a numeric conversion reads this value out of.
    ///
    /// [`Value::as_text`] for everything but a document, where MySQL converts
    /// the *scalar* and not the rendering: `CAST(CAST('"2.9"' AS JSON) AS
    /// DECIMAL(10,2))` is 2.90 on 8.3, where reading `"2.9"` — quotes and all
    /// — as digits gives 0.00. See [`crate::json::numeric_text`] and #0084.
    ///
    /// A decimal is the other value that parts company with its rendering
    /// here, and in the other direction: it converts by the digits it
    /// *carries*, where `as_text` answers the ones it shows. Measured on 8.3,
    /// `CAST(1/3 AS DECIMAL(20,9))` is 0.333333333 and `INSERT ... VALUES
    /// (1/3)` into a `DECIMAL(20,9)` reads back the same, while `CAST(1/3 AS
    /// CHAR)` is 0.3333 — see [`Value::Decimal`] and #0118.
    ///
    /// For the conversions that go through characters rather than through a
    /// double: a DECIMAL cast and a DECIMAL column, both of which keep the
    /// text precisely because parsing it would lose precision.
    pub fn numeric_text(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Value::Json(document) => crate::json::numeric_text(document),
            Value::Decimal(digits, _) => std::borrow::Cow::Borrowed(digits),
            other => std::borrow::Cow::Owned(other.as_text()),
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
            Value::Text(v, _) => v.clone(),
            Value::Bytes(v) => String::from_utf8_lossy(v).into_owned(),
            // The text it was stored as: only the *numeric* reading of a
            // temporal is packed, and `SELECT d` still shows `2024-01-05`.
            Value::Temporal(v) => v.clone(),
            // The member, not the ordinal: only the *numeric* reading of an
            // ENUM is its position, and `SELECT e` still shows `a`.
            Value::Enum { member, .. } => member.clone(),
            // A decimal reads as the characters it is *shown* as, not as the
            // digits it carries: measured on 8.3, `CAST(1/3 AS CHAR)` and
            // `CONCAT(1/3, '')` are both `0.3333` where the value behind them
            // is 0.333333333. `LENGTH(1/3)` is 6 there, which counts the same
            // six characters.
            Value::Decimal(..) => self.displayed().into_owned(),
            // The document as MySQL writes it, which is what it sends and what
            // a client reads back. Not the stored text: a decimal inside one
            // is *held* wearing a mark that says it is a decimal and not a
            // double, and this is the boundary that takes it off again — see
            // `json::DECIMAL_MARK`.
            Value::Json(v) => crate::json::wire_text(v).into_owned(),
            // `1` and `0`. MySQL has no boolean on the wire — `SELECT TRUE`
            // and `SELECT 1 = 1` both send the character `1` — so the tag is
            // invisible to a client and shows up only inside a document.
            Value::Bool(b) => i64::from(*b).to_string(),
            // Without the sign the `i64` reading would put on it: the whole
            // point of the tag is that 18446744073709551615 is not -1.
            Value::Unsigned(v) => v.to_string(),
        }
    }

    /// The value's *bytes*, which for everything but a binary string are the
    /// bytes of the text it prints as.
    ///
    /// Not `as_text().into_bytes()`: `as_text` decodes a `Bytes` lossily, so
    /// every byte that is not valid UTF-8 becomes U+FFFD and `UNHEX('FF')`
    /// and `UNHEX('FE')` arrive here identical. The whole point of asking for
    /// bytes is to get the ones that are there.
    ///
    /// Borrowed where there is nothing to build, because this runs per row per
    /// comparison.
    pub(crate) fn as_bytes(&self) -> std::borrow::Cow<'_, [u8]> {
        match self {
            Value::Bytes(bytes) => std::borrow::Cow::Borrowed(bytes),
            other => std::borrow::Cow::Owned(other.as_text().into_bytes()),
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
            Value::Text(v, _) => Some(numeric_prefix(v) != 0.0),
            Value::Bytes(v) => Some(numeric_prefix(&String::from_utf8_lossy(v)) != 0.0),
            // Read like the text it used to be, so tagging a value changed
            // nothing about how a WHERE clause treats it.
            Value::Decimal(v, _) => Some(numeric_prefix(v) != 0.0),
            // And a document by the number read out of its *scalar*, which is
            // 8.3's answer for `IF(j, 1, 0)`: 1 for `"2026"`, `"2.5e3"`, `3`
            // and `true`, and 0 for `"abc"`, `"0"`, `null`, `false`, `[]` and
            // `{}`. Read out of the rendering the quoted numerics were 0 too.
            // See #0084.
            //
            // `WHERE j` is a *different* question on 8.3, and this does not
            // answer it: there every document but the JSON number 0 is true,
            // `"abc"` and `null` included. One `truthy` cannot be both, and
            // the `IF` reading is the one the rest of this file agrees with.
            // Measured, and a divergence of its own.
            Value::Json(_) => Some(self.as_number() != 0.0),
            // By the packed number, for the same reason arithmetic is: MySQL
            // asks a temporal the numeric question the way it converts one.
            Value::Temporal(_) => Some(self.as_number() != 0.0),
            // By the ordinal, so `WHERE e` is true for every real member and
            // false only for the error value. Read as text, the first member
            // of `ENUM('a','b')` was false.
            Value::Enum { ordinal, .. } => Some(*ordinal != 0),
            Value::Bool(b) => Some(*b),
            Value::Unsigned(v) => Some(*v != 0),
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

        // Two integers compare as integers, ahead of the `f64` path below,
        // because that path cannot tell 18446744073709551615 from
        // 18446744073709551614 — both round to 2^64 as doubles, so
        // `bu = 18446744073709551615` answered 1 for either. Widened to
        // `i128`, where every `i64` and every `u64` is exact, so `bu > -1` is
        // decided by the numbers rather than by the sign. An `Int` pair was
        // already handled below the `f64` path and now reaches this one
        // instead, which is the same answer for every value a double can hold
        // and a better one above 2^53.
        if let (Some(a), Some(b)) = (self.exact_integer(), other.exact_integer()) {
            return Some(a.cmp(&b));
        }

        // And a decimal beside either of those, for the same reason one step
        // further out: an `i128` is 38 digits where MySQL's DECIMAL is 65, and
        // an `f64` gives up at seventeen. Two `DECIMAL(65,0)` columns holding
        // 10000000000000000000000000000000000001 and ...000 compared *equal*
        // through the `f64` path while `a - b` answered 1 — one row saying two
        // things — and `18446744073709551616 = 18446744073709551615` was 1
        // under columns that print all twenty digits.
        //
        // Reached only where at least one side is a `Decimal`, since the
        // integer pair returned above. It has to agree with `collation_key`,
        // which keys a decimal by its number at every width: without this,
        // `GROUP BY` and `=` disagreed about the same two values.
        if let (Some(a), Some(b)) = (self.exact_decimal(), other.exact_decimal()) {
            if let Some(order) = crate::decimal::compare(&a, &b) {
                return Some(order);
            }
        }

        // Numbers compare numerically even against numeric strings, which is
        // how MySQL behaves and what Prisma's generated comparisons rely on.
        // A decimal is a number: `2.50` equals `2.5` and sorts below `10.00`,
        // where two *strings* of those characters do neither.
        // A boolean is one of them: `TRUE = '1'` is 1 on 8.3 and `TRUE =
        // 'abc'` is 0, which is the numeric reading and not the collation's.
        // Left out, the pair fell through to `collate` and answered by the
        // characters.
        let both_numeric = matches!(
            self,
            Value::Int(_)
                | Value::Float(_)
                | Value::Decimal(..)
                | Value::Float32(_)
                | Value::Bool(_)
                | Value::Unsigned(_)
        ) || matches!(
            other,
            Value::Int(_)
                | Value::Float(_)
                | Value::Decimal(..)
                | Value::Float32(_)
                | Value::Bool(_)
                | Value::Unsigned(_)
        );

        if both_numeric {
            if let (Some(a), Some(b)) = (self.as_float(), other.as_float()) {
                return a.partial_cmp(&b);
            }
        }

        match (self, other) {
            // Binary data is compared byte-exact, and *one* binary operand is
            // enough. MySQL resolves a comparison's collation from both sides,
            // and the binary character set wins over any other: measured on
            // 8.3, `COLLATION(CAST('AB' AS BINARY))` is `binary` with
            // coercibility 2 against `utf8mb4_0900_ai_ci` at 4 for a literal,
            // so the pair is compared as bytes and `CAST('AB' AS BINARY) =
            // 'ab'` is 0. Requiring *both* sides to be `Bytes` sent that pair
            // to `collate` instead, which folds case, and it answered 1 —
            // along with `X'4142' = 'ab'` and a VARBINARY column against a
            // string literal. See #0139.
            //
            // Below the numeric paths above on purpose: a binary string beside
            // a number is still a numeric comparison there, so
            // `CAST('12' AS BINARY) = 12` is 1 and not a comparison of `"12"`
            // against `"12"` that would only agree by accident.
            (Value::Bytes(_), _) | (_, Value::Bytes(_)) => {
                Some(self.as_bytes().cmp(&other.as_bytes()))
            }
            _ => Some(
                self.collation_with(other)
                    .compare(&self.as_text(), &other.as_text()),
            ),
        }
    }

    /// The collation a comparison of these two operands is made under.
    ///
    /// Lenient where MySQL raises 1267, and deliberately: `compare` answers
    /// `Option<Ordering>` for the three-valued logic and has nowhere to put an
    /// error, and it is reached from sorting, grouping and foreign-key
    /// matching as well as from `=`. The refusal is raised where a statement
    /// can carry it — [`crate::expr`] checks the pair before comparing it — so
    /// the fallback here is only ever reached by a path 8.3 does not refuse.
    pub(crate) fn collation_with(&self, other: &Value) -> Collation {
        crate::collation::resolve(self.collated_as(), other.collated_as())
            .unwrap_or(Collation::Ai0900Ci)
    }

    /// The order MySQL *sorts* this value in, which for an ENUM is not the
    /// order it *compares* in.
    ///
    /// Both halves are measured on 8.3, and they disagree. `ORDER BY e` over
    /// `ENUM('b','a')` returns `b` before `a` — the declaration order, not the
    /// alphabet — while `e1 < e2` over two columns of that same type answers
    /// by the members: `'b' < 'a'` is false there where the ordinals 1 < 2
    /// would make it true. `MIN` and `MAX` go with the comparison, not with
    /// the sort, so `MIN(e)` over those two rows is `a`.
    ///
    /// So the sort cannot share `compare`, and only the pairing where both
    /// sides are ENUM members is decided here; everything else, an ENUM beside
    /// a string or a number included, is ordered the way it is compared.
    ///
    /// Two ordinals are only comparable when they count from the same
    /// declaration, which is not written on the value. What made them meet was
    /// a set operation — position 1 is `b` in `ENUM('b','a')` and `a` in
    /// `ENUM('a','b','c')` — and MySQL does not carry an ENUM through one at
    /// all, so the engine untags there and the pairing no longer arises. A sort
    /// key is otherwise one expression evaluated per row, so both sides of it
    /// come from the same column. `IF(c, e1, e2)` over two declarations is the
    /// gap that remains, and MySQL answers that by the chosen argument's own
    /// ordinal too — measured on 8.3 — so there is no obviously better answer
    /// to give it than this one.
    pub fn sort_cmp(&self, other: &Value) -> Option<Ordering> {
        match (self, other) {
            (Value::Enum { ordinal: a, .. }, Value::Enum { ordinal: b, .. }) => Some(a.cmp(b)),
            _ => self.compare(other),
        }
    }

    /// This value with any ENUM tag removed, leaving the member as text.
    ///
    /// For the places MySQL stops reading a value as its ordinal. It does not
    /// do so everywhere: `IF`, `CASE` and `NULLIF` hand the ENUM through and
    /// `IF(1, e, 'z') + 0` is still the ordinal, while `COALESCE`, `IFNULL`,
    /// `GREATEST`, `LEAST`, `MIN` and `MAX` build a string result and
    /// `COALESCE(e) + 0` is 0 — with one argument, so it is the function and
    /// not a mixture of types that does it. All measured on 8.3.
    ///
    /// The member comes out at NUMERIC strength rather than COERCIBLE, which
    /// is the same "holds no collation of its own" [`Value::collated_as`]
    /// gives the tagged form: this engine's `Value::Enum` does not carry the
    /// column's collation, so the untagged member has none to keep either, and
    /// it must *lose* to whatever it meets rather than tie with it. Tying is a
    /// 1267 — `SELECT (SELECT e FROM t) = 'b'` raised one here where 8.3
    /// answers a row, because a literal now carries the connection's collation
    /// and this carried the server's. Losing gives the literal's, which is
    /// 8.3's answer whenever the column's collation and the connection's
    /// agree; where they do not, 8.3 uses the column's and this does not. That
    /// residue is an ENUM that cannot say what it is collated as, not this
    /// rule.
    pub fn untag_enum(self) -> Value {
        match self {
            Value::Enum { member, .. } => Value::Text(member, Collated::NUMERIC),
            other => other,
        }
    }

    /// This value with any boolean tag removed, leaving 1 or 0.
    ///
    /// For the places MySQL stops reading a value as a boolean, which are not
    /// the places it stops reading an ENUM as its ordinal — the two lists were
    /// measured separately and overlap only by accident. `IF`, `CASE` and
    /// `NULLIF` keep the ordinal and *drop* the boolean, so
    /// `JSON_ARRAY(IF(1, TRUE, FALSE))` is `[1]` on 8.3; a scalar subquery
    /// drops the ordinal and *keeps* the boolean, so `(SELECT TRUE) =
    /// CAST('true' AS JSON)` is 1. `COALESCE`, `IFNULL`, `GREATEST`, `LEAST`,
    /// `MIN`, `MAX` and a set operation drop both.
    ///
    /// The rule underneath all of it: a boolean survives only inside the
    /// expression that asked the question. Anything that gives it a column to
    /// live in — a derived table, a CTE, a user variable, a real column —
    /// takes the tag off, so `SELECT JSON_ARRAY(x) FROM (SELECT TRUE AS x) t`
    /// is `[1]` while the bare `JSON_ARRAY(TRUE)` is `[true]`.
    pub fn untag_bool(self) -> Value {
        match self {
            Value::Bool(b) => Value::Int(i64::from(b)),
            other => other,
        }
    }

    /// This value with any UNSIGNED tag removed, leaving the signed reading.
    ///
    /// For the places MySQL's own result type is signed although an unsigned
    /// operand reached them, and the list is its own — measured on 8.3, and
    /// not the list `untag_bool` or `untag_enum` above carry. Unary minus
    /// drops it, so `-iu - 100` is -107 and not a refusal. A conditional needs
    /// *every* candidate to be unsigned, so `COALESCE(iu, 1) - 100` is -93
    /// where `COALESCE(iu, iu) - 100` raises 1690 — the same all-candidates
    /// rule the UNSIGNED flag on the wire follows. A set operation keeps it
    /// only where both branches have it, which is where a scalar subquery, a
    /// derived table and `NULLIF` differ: all three keep it.
    ///
    /// Widened rather than wrapped, because none of those places is a signed
    /// *integer*: MySQL has nothing 64-bit that holds both a `u64` and a
    /// negative, so it answers a DECIMAL and keeps every digit —
    /// `COALESCE(bu, b)` over 18446744073709551615 is that number, NEWDECIMAL,
    /// on 8.3, and `SELECT ~0 UNION SELECT 1` is that number beside 1. Wrapped
    /// to an `i64` both of those read -1.
    ///
    /// A cast is the one place that really does wrap — `CAST(bu AS SIGNED)` is
    /// -1 there — and it says so in the statement rather than coming through
    /// here.
    pub fn untag_unsigned(self) -> Value {
        match self {
            Value::Unsigned(v) => match i64::try_from(v) {
                Ok(n) => Value::Int(n),
                Err(_) => Value::decimal(v.to_string()),
            },
            other => other,
        }
    }

    /// The exact integer this value *is*, for the two tags that hold one.
    ///
    /// `i128` because the two ranges do not fit in either 64-bit type: an
    /// `Int` reaches below zero and an `Unsigned` above `i64::MAX`. `None` for
    /// everything else, including a numeric string — those still compare
    /// through `f64`, which is what they did before there was an unsigned tag.
    fn exact_integer(&self) -> Option<i128> {
        match self {
            Value::Int(v) => Some(i128::from(*v)),
            Value::Unsigned(v) => Some(i128::from(*v)),
            _ => None,
        }
    }

    /// The exact decimal text this value *is*, for the three tags that hold
    /// one.
    ///
    /// The operand set the exact arithmetic in `expr.rs` works over, and the
    /// one `compare` above widens to: an integer of either sign is a decimal
    /// with no places. `None` for everything else, a numeric string included —
    /// MySQL reads one of those as a double, so answering it exactly here
    /// would make `d = '2.5'` a different question from `d + '2.5'`.
    ///
    /// Borrowed where there is text to borrow, because this runs per row per
    /// comparison and a `Decimal` already holds its digits.
    pub(crate) fn exact_decimal(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Value::Int(v) => Some(std::borrow::Cow::Owned(v.to_string())),
            Value::Unsigned(v) => Some(std::borrow::Cow::Owned(v.to_string())),
            Value::Decimal(text, _) => Some(std::borrow::Cow::Borrowed(text)),
            _ => None,
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
            Value::decimal(format!("{sign}{integer}.{fraction}"))
        })
    }

    /// This value as the JSON operand MySQL would compare it as.
    ///
    /// A non-document beside a document becomes a JSON *scalar* of its own
    /// type — a string becomes a JSON string, which is why `doc = '{"a": 1}'`
    /// is false for every row: the literal is the string `{"a": 1}` and an
    /// object never equals a string.
    ///
    /// A boolean becomes the JSON *boolean*, which is the case this used to
    /// say was out of reach: MySQL reads the keyword `TRUE` as JSON `true`
    /// and the literal `1` as the number 1, so `CAST('true' AS JSON) = TRUE`
    /// is 1 and `= 1` is 0. Both operands were `Value::Int(1)` until
    /// `Value::Bool` existed to tell them apart. See #0061.
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
    ///
    /// The binary boundary is the same shape and for the same reason. Since
    /// #0139 `compare` reads a `Bytes` beside a `Text` byte-exactly, and the
    /// keys below still fold the text's case, so `UNHEX('6162')` and `'AB'`
    /// key together where `=` now separates them: `SELECT UNHEX('6162') UNION
    /// SELECT 'AB'` is one row here and two on 8.3. A plain UNION is enough to
    /// see it, so this is a wrong answer and not only a theoretical gap.
    ///
    /// Keying the two apart is not the fix, and that was measured rather than
    /// argued: it would have to key `UNHEX('6162')` apart from `'ab'` and `2`
    /// apart from `CAST('2' AS BINARY)` as well, and 8.3 puts *both* of those
    /// pairs in one group. What MySQL does instead is resolve one collation
    /// for the whole key *expression* — every value under one key is binary or
    /// none is — which is #0128's gap and not something a per-value key can
    /// reach. Fixing it here would trade four measured wrong answers for four
    /// others.
    ///
    /// A column whose values are all of one kind is unaffected either way, and
    /// that is every column a table produces.
    pub fn collation_key(&self) -> String {
        match self {
            // Case kept, unlike the text arm at the bottom: bytes carry no
            // collation to fold them under, so `'AB'` and `'ab'` in a
            // VARBINARY column are two groups and two unique keys — which is
            // what `compare` has said about them since #0139.
            //
            // And bytes that are not text at all get a key of their own,
            // because a lossy decode gives every one of them U+FFFD: over a
            // VARBINARY column holding `UNHEX('FF')` and `UNHEX('FE')`,
            // `COUNT(DISTINCT b)` was 1 and `GROUP BY b` gave one group where
            // 8.3 gives two — while `=` called the pair different. The same
            // key drives UNION's de-duplication and foreign-key matching, so
            // an FK child on one of those bytes matched a parent on the other.
            //
            // Only bytes that need it are escaped, so a byte string that *is*
            // text keys as that text and goes on grouping with the `'ab'` and
            // the `2` that MySQL puts in a group with it — see the note above.
            // The marker is what keeps the escape out of every other arm's key
            // space: a text key has been lowercased and a number's is digits,
            // so neither can contain an upper-case `X`.
            //
            // A byte string that is valid UTF-8 and *starts with a NUL* is
            // escaped too, because NUL is what every sentinel in this engine
            // is built out of and a byte string can spell any of them.
            // `UNHEX('00584333')` reads as `\0XC3`, the escape for the single
            // byte `0xC3`; `UNHEX('004E554C4C')` reads as `\0NULL`, which is
            // the key `exec.rs` gives SQL NULL in a GROUP BY, in a window's
            // PARTITION BY and in a set operation's row key. That one folded a
            // NULL row into the value's group and *reported* the group as
            // holding those bytes, while `IS NULL` still said it was NULL.
            //
            // Guarding on the NUL rather than on the marker keeps every
            // property the escape already had — an escaped key still begins
            // with the marker, and hex is injective — and adds the one that
            // makes them hold: no *unescaped* byte key can begin with a NUL,
            // so no byte string can spell a sentinel at all.
            //
            // It costs one case of the mixed-kind gap above, and that is the
            // trade rather than a free win. A NUL-led byte string no longer
            // groups with the identical *text*, where 8.3 does and where the
            // engine did: measured, `UNHEX('00616263')` against
            // `CAST(UNHEX('00616263') AS CHAR)` is one row there and two here,
            // beside the `'abc'` pair that still groups. Splitting a mixed
            // UNION is the same #0128 gap one case wider; reporting a NULL row
            // as holding bytes is a value that is not there.
            Value::Bytes(bytes) => match std::str::from_utf8(bytes) {
                Ok(text) if !text.starts_with('\u{0}') => text.to_owned(),
                _ => {
                    let mut key = String::with_capacity(BYTES_KEY_MARKER.len() + bytes.len() * 2);
                    key.push_str(BYTES_KEY_MARKER);
                    for byte in bytes {
                        // `write!` rather than `push_str(&format!(..))`, which
                        // allocates a `String` per byte. This runs per row per
                        // group key, and a BLOB has a lot of bytes.
                        let _ = write!(key, "{byte:02X}");
                    }
                    key
                }
            },
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
            Value::Decimal(text, _) => match crate::decimal::shortest(text) {
                Some(key) => key,
                // A `Decimal` whose text is not one, which nothing validates.
                None => text.to_lowercase(),
            },
            // Under the column's own collation, which is what makes DISTINCT,
            // GROUP BY and a unique key agree with `=` about `café` and
            // `Cafe`. It used to be `to_lowercase` for every column alike,
            // which is the same one-rule-for-four the comparison had.
            Value::Text(text, collated) => collated.collation.key(text),
            other => Collation::Ai0900Ci.key(&other.as_text()),
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
            Value::Text(v, _) => WireValue::Str(v.clone()),
            Value::Bytes(v) => WireValue::Bytes(v.clone()),
            // A decimal travels as its text, which is how MySQL sends one.
            // A temporal does too — the packed number is a reading, not a
            // representation, and a client asking for a DATE gets its text.
            Value::Temporal(v) => WireValue::Str(v.clone()),
            // At the scale it is described with, which is where a quotient's
            // extra places stop: `SELECT 1/3` sends `0.3333` on 8.3 for a
            // value of 0.333333333. This is the edge both protocols leave
            // through, so it is the one place the rounding has to happen.
            Value::Decimal(..) => WireValue::Str(self.displayed().into_owned()),
            // JSON crosses the wire as text, which is how MySQL transmits it —
            // through `as_text`, so the stored form's decimal mark comes off
            // here exactly as it does in every other rendering of a document.
            Value::Json(_) => WireValue::Str(self.as_text()),
            // The member. A client asking for an ENUM column gets `a`, which
            // is what MySQL sends for it; the ordinal is a reading, not a
            // representation.
            Value::Enum { member, .. } => WireValue::Str(member.clone()),
            // As the integer it announces itself to be. `SELECT TRUE` is a
            // LONGLONG column on 8.3 carrying `1`, so a binary client reads
            // eight bytes; sending the tag as a string would have desynced it
            // against the type code beside it.
            Value::Bool(b) => WireValue::I64(i64::from(*b)),
            // As a `u64`, which the wire encoding already has a case for. Sent
            // as an `i64` it would reach a binary client as -1 under a column
            // the engine has just announced UNSIGNED.
            Value::Unsigned(v) => WireValue::U64(*v),
        }
    }

    /// Converts a bound parameter from the wire.
    pub fn from_wire(value: &WireValue) -> Self {
        match value {
            WireValue::Null => Value::Null,
            WireValue::I64(v) => Value::Int(*v),
            // Only where it will not fit signed, which keeps every parameter
            // that already worked exactly as it was: a client sends most
            // integers as `U64` whatever their size, and tagging all of them
            // would make `? - 100` refuse for a bound 7. Above `i64::MAX`
            // there is no such choice — `Int(*v as i64)` made
            // 18446744073709551615 into -1, so a `BIGINT UNSIGNED` column that
            // takes the value through the text protocol refused it through the
            // binary one.
            WireValue::U64(v) => match i64::try_from(*v) {
                Ok(n) => Value::Int(n),
                Err(_) => Value::Unsigned(*v),
            },
            WireValue::F32(v) => Value::Float(*v as f64),
            WireValue::F64(v) => Value::Float(*v),
            WireValue::Str(v) => Value::text(v.clone()),
            WireValue::Bytes(v) => Value::Bytes(v.clone()),
        }
    }
}

/// What [`Value::collation_key`] prefixes a byte string it had to escape.
///
/// A NUL because that is how this engine spells a key that is not a value —
/// `exec.rs` keys SQL NULL as `\0NULL` — and an upper-case `X` so an escaped
/// byte string cannot be taken for one of those either: every other kind of
/// value keys through `to_lowercase` or through digits, and no sentinel here
/// carries an `X` in that position.
///
/// The other half of the guarantee is at the use site, which escapes *any*
/// byte string whose text begins with a NUL rather than only one beginning
/// with this marker. Without that, bytes spelling `\0NULL` keyed as the NULL
/// sentinel itself.
const BYTES_KEY_MARKER: &str = "\u{0}X";

/// Renders a float the way MySQL does: no trailing `.0`, no exponent for
/// ordinary magnitudes.
fn format_float(v: f64) -> String {
    // One formatter for both crates: a value rendered here and the same value
    // rendered on its way out have to agree, and they did not — this dropped a
    // trailing `.0` and never reached for an exponent, so `1e20` was twenty-one
    // digits where MySQL writes `1e20`.
    mysql_ultralight_protocol::value::format_float(v, None)
}

/// Digits as a whole number, the way [`Value::as_int`] reads a value that is
/// held as characters.
///
/// Every such value takes this reading — text, a decimal, a binary string and
/// the scalar inside a document — and they have to take the *same* one: a
/// value an `INT` column refuses with 1366 and the `DOUBLE` beside it accepts
/// is a difference between two copies of this, not a rule MySQL has.
///
/// The integer parse comes first so that digits an `f64` cannot hold exactly
/// are not rounded on the way through; the float is the fallback for anything
/// with a point or an exponent in it.
fn digits_as_int(text: &str) -> Option<i64> {
    let t = text.trim();
    t.parse::<i64>()
        .ok()
        .or_else(|| t.parse::<f64>().ok().map(|f| f as i64))
}

/// The same reading at the width [`Value::as_exact_int`] needs, where every
/// `i64` and every `u64` is exact.
fn digits_as_exact_int(text: &str) -> Option<i128> {
    let t = text.trim();
    t.parse::<i128>()
        .ok()
        .or_else(|| t.parse::<f64>().ok().map(|f| f as i128))
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
            Some(Value::decimal("20240105060708.500"))
        );
        assert_eq!(packed("06:07:08.5"), Some(Value::decimal("60708.5")));
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
