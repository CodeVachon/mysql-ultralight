//! Fixed-point decimals, kept as text and counted in units of the last place.
//!
//! MySQL's DECIMAL is exact and carries a scale: a `DECIMAL(10,2)` holding 1.5
//! *is* `1.50`, and every rule that touches it is written in terms of that
//! scale rather than of the number. Folding one through an `f64` loses both
//! halves of that — the digits and the count of them — so `SUM` of `1.50` and
//! `2.50` came back `4` where MySQL says `4.00`.
//!
//! The representation stays text, because that is how a decimal arrives on the
//! wire and how it leaves. What is added here is arithmetic on the *units* of
//! the last place, which keeps the scale explicit rather than implied by
//! whatever a float printed.
//!
//! Two counts of those units live here. [`units`] and [`render`] carry them in
//! an `i128`, which the aggregates use because a fold is a running total and a
//! machine integer is the cheap way to keep one. The operators use the digit
//! strings below instead, and they have to: an `i128` is 38 digits where
//! MySQL's DECIMAL is 65, so a `DECIMAL(30,10)` times a `DECIMAL(65,30)`
//! overflows one and is an ordinary value to the other — and the fallback for
//! an overflow was the `f64` path, which is how a product of two exact
//! operands came back 5.25 for 5.250000000000000000000000000000.

/// How many digits a decimal's text carries after the point.
pub fn scale_of(text: &str) -> usize {
    match text.trim().split_once('.') {
        Some((_, fraction)) => fraction.chars().filter(char::is_ascii_digit).count(),
        None => 0,
    }
}

/// A decimal's digits as an integer count of units at `scale`.
///
/// `None` for anything that is not a plain decimal — an exponent, a name, an
/// empty string — and for a number too long to count in an `i128`, which is
/// 38 digits and past anything MySQL's own DECIMAL holds.
pub fn units(text: &str, scale: usize) -> Option<i128> {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix(['-', '+']) {
        Some(rest) => (text.starts_with('-'), rest),
        None => (false, text),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let mut value: i128 = whole
        .parse()
        .ok()
        .or(if whole.is_empty() { Some(0) } else { None })?;
    // The fraction, digit by digit, so a scale shorter than the text rounds
    // rather than truncating: MySQL stores 1.005 in a `DECIMAL(10,2)` as 1.01.
    let taken: Vec<u32> = fraction.chars().filter_map(|c| c.to_digit(10)).collect();
    for i in 0..scale {
        value = value.checked_mul(10)?;
        value = value.checked_add(*taken.get(i).unwrap_or(&0) as i128)?;
    }
    if taken.get(scale).is_some_and(|d| *d >= 5) {
        value = value.checked_add(1)?;
    }
    Some(if negative { -value } else { value })
}

/// `text` at exactly `scale` digits, worked on the characters.
///
/// [`rescale`] counts in an `i128` and runs out at 38 digits — and spends the
/// budget on the scale before the whole part is reached at all, so a
/// `DECIMAL(40,30)` overflows at ten integer digits. MySQL's DECIMAL is 65
/// digits and 30 places, so those are values it holds and answers exactly,
/// and folding them through an `f64` answered `i128::MAX`'s digits instead —
/// a number nothing asked for.
///
/// Rounds half away from zero on the first digit dropped, which is the rule
/// [`units`] applies rather than "round what is left over": 1.0049 at two
/// places is 1.00 both there and here.
///
/// `None` for text that is not a decimal at all. That case has a correct
/// answer of its own — the number MySQL reads out of it — and is not this
/// function's to give.
pub fn rescale_wide(text: &str, scale: usize) -> Option<String> {
    let trimmed = text.trim();
    let (negative, rest) = match trimmed.strip_prefix(['-', '+']) {
        Some(rest) => (trimmed.starts_with('-'), rest),
        None => (false, trimmed),
    };
    let (whole, fraction) = rest.split_once('.').unwrap_or((rest, ""));
    let plain = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if !plain(whole) || !plain(fraction) || (whole.is_empty() && fraction.is_empty()) {
        return None;
    }

    // Whole part then the fraction padded out to the scale, as one run of
    // digits with the point's position implied by `scale`.
    let mut digits: Vec<u8> = whole.bytes().collect();
    for i in 0..scale {
        digits.push(*fraction.as_bytes().get(i).unwrap_or(&b'0'));
    }
    if fraction.as_bytes().get(scale).is_some_and(|d| *d >= b'5') {
        // Carried through the string rather than added, because 9.99 at one
        // place is 10.0 and the carry can run off the front into a new digit.
        let mut at = digits.len();
        loop {
            if at == 0 {
                digits.insert(0, b'1');
                break;
            }
            at -= 1;
            if digits[at] == b'9' {
                digits[at] = b'0';
            } else {
                digits[at] += 1;
                break;
            }
        }
    }

    for digit in &mut digits {
        *digit -= b'0';
    }
    Some(spell(negative, &digits, scale))
}

/// A run of digits and a point position, written out as a decimal.
///
/// The digits are values, not characters, because that is what the arithmetic
/// below carries them as; `rescale_wide` subtracts `b'0'` to reach here rather
/// than keeping a second spelling of this.
fn spell(negative: bool, digits: &[u8], scale: usize) -> String {
    let all = padded(digits, scale);
    let split = all.len() - scale;
    let mut written = String::with_capacity(all.len() + 2);
    for (at, digit) in all.iter().enumerate() {
        // Leading zeros are not written, but the last digit before the point
        // is not a leading zero however small the number is.
        if written.is_empty() && *digit == 0 && at + 1 < split {
            continue;
        }
        if at == split {
            written.push('.');
        }
        written.push(char::from(b'0' + digit));
    }
    // A value that came out zero has no sign, which is `render`'s answer too:
    // -0.4 at scale zero is `0` and not `-0`.
    if negative && all.iter().any(|d| *d != 0) {
        written.insert(0, '-');
    }
    written
}

/// `digits` with enough leading zeros to carry `scale` places and one digit in
/// front of them.
///
/// Every operator can hand back fewer digits than its scale — a remainder of
/// nothing is the extreme — and both readers need the same answer for it:
/// spelled, that is `0.000` rather than `0`, and rounded, it is a number
/// smaller than the place being rounded at rather than one whose leading digit
/// happens to sit there.
fn padded(digits: &[u8], scale: usize) -> Vec<u8> {
    let padding = (scale + 1).saturating_sub(digits.len());
    std::iter::repeat_n(0, padding)
        .chain(digits.iter().copied())
        .collect()
}

/// The largest scale a MySQL DECIMAL *column* carries, and the ceiling a
/// product or a quotient is clamped to.
///
/// Only those two. Measured on 8.3 over a literal written to 37 places, which
/// is the one way a scale past 30 gets into an expression at all: `+`, `-`,
/// `%` and a sign all keep every one of the 37, while `* 1` and `/ 1` come
/// back at 30 — MySQL clamps in the two operators whose rule *adds* scales and
/// nowhere else, and this follows it rather than tidying the difference away.
///
/// The *precision* has a documented maximum of 65 and is deliberately not
/// clamped here, because MySQL does not clamp the value either: two 35-digit
/// `DECIMAL(65,0)` operands multiply to a 70-digit answer there, printed in
/// full under a column still described `decimal(65,0)`.
pub const MAX_SCALE: usize = 30;

/// The extra decimal places a quotient gains over its left operand, for a
/// session that has not said otherwise.
///
/// `div_precision_increment`'s documented default, measured as 4 on 8.3. A
/// session can set it anywhere in 0..=30, so every caller here takes the
/// increment as an argument and only [`Session::div_precision_increment`]
/// falls back to this.
///
/// An average takes the same number over the scale it averaged, which is why
/// there is one constant and not two: `AVG` of a `DECIMAL(10,2)` is a
/// `DECIMAL(_,6)` and `AVG` of an integer is a `DECIMAL(_,4)`, both moving
/// with the variable.
///
/// [`Session::div_precision_increment`]: crate::session::Session::div_precision_increment
pub const DEFAULT_DIV_PRECISION_INCREMENT: usize = 4;

/// The largest `div_precision_increment` MySQL will hold.
///
/// Measured on 8.3: 31 is stored as 30 and -1 as 0, each with warning 1292,
/// so the range is 0..=30 and out of range is clamped rather than refused.
pub const MAX_DIV_PRECISION_INCREMENT: i128 = 30;

/// A decimal with the point taken out: its sign, its digits, and how many of
/// them fall after the point.
struct Parts {
    negative: bool,
    digits: Vec<u8>,
    scale: usize,
}

impl Parts {
    /// Whether this is zero, however many places it is spelled to. The
    /// divisor test for `/`, `%` and `DIV`, all three of which answer NULL
    /// rather than a number.
    fn is_zero(&self) -> bool {
        self.digits.iter().all(|d| *d == 0)
    }
}

/// `text` split up, or `None` if it is not a plain decimal.
///
/// An exponent is not one: MySQL types `1e3` DOUBLE and computes over it
/// approximately, so reading it here would answer a whole family of
/// expressions exactly that MySQL does not.
fn parts(text: &str) -> Option<Parts> {
    let trimmed = text.trim();
    let (negative, rest) = match trimmed.strip_prefix(['-', '+']) {
        Some(rest) => (trimmed.starts_with('-'), rest),
        None => (false, trimmed),
    };
    let (whole, fraction) = rest.split_once('.').unwrap_or((rest, ""));
    let plain = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if !plain(whole) || !plain(fraction) || (whole.is_empty() && fraction.is_empty()) {
        return None;
    }
    Some(Parts {
        negative,
        digits: whole
            .bytes()
            .chain(fraction.bytes())
            .map(|b| b - b'0')
            .collect(),
        scale: fraction.len(),
    })
}

/// `text` without its sign, keeping the scale.
///
/// `ABS` is the same rule as the unary minus — a sign says nothing about how
/// many places a number is written to — and MySQL agrees: `ABS(-1.50)` is 1.50
/// on 8.3 and is declared `decimal(3,2)`.
pub fn absolute(text: &str) -> Option<String> {
    let value = parts(text)?;
    Some(spell(false, &value.digits, value.scale))
}

/// Whether `text` is zero, however many places it is spelled to.
///
/// The divisor test for `/`, `%` and `DIV`, all three of which MySQL answers
/// NULL rather than refusing: `1/0`, `1.5 MOD 0` and `1 DIV 0` are NULL on 8.3
/// under every `sql_mode` a `SELECT` can be run in, strict included.
pub fn is_zero(text: &str) -> bool {
    parts(text).is_some_and(|value| value.is_zero())
}

/// -1, 0 or 1 for a plain decimal, read off its digits rather than an `f64`.
///
/// `SIGN` is a question about a decimal that an approximation can get wrong in
/// both directions: `0.1 + 0.2 - 0.3` is exactly zero here and 5.55e-17 as a
/// double, and 8.3 answers 0 for the decimal and 1 for the same sum written
/// with `e0` exponents. Reading the sign off the digits is what keeps those
/// two apart.
///
/// Zero has no sign, which is the same rule [`compare`] holds to: `-0.00` is
/// 0, not -1, and 8.3 agrees — `SIGN(-0.00)` is 0 there. A decimal that
/// rounded to zero on the way into its column has genuinely become zero, so a
/// `DECIMAL(6,2)` holding what was written as -0.001 is 0 too.
///
/// `None` for anything that is not a plain decimal; those read as numbers,
/// which is a different question with a different answer.
pub fn sign(text: &str) -> Option<i64> {
    parts(text).as_ref().map(sign_of)
}

/// The sign of an already-parsed decimal.
fn sign_of(value: &Parts) -> i64 {
    if value.is_zero() {
        0
    } else if value.negative {
        -1
    } else {
        1
    }
}

/// `a` against `b`, exactly, over two decimals.
///
/// The comparison to match the arithmetic. Read through an `f64` a decimal
/// loses everything past the seventeenth significant digit, so two
/// `DECIMAL(65,0)` columns holding 10000000000000000000000000000000000001 and
/// ...000 compared *equal* while their difference was 1 — one row saying two
/// things. MySQL answers 0 for `a = b` and 1 for `a > b`, and this does too.
///
/// Zero has no sign, so `-0.00` and `0` are one value however they were
/// written — which is also what [`shortest`] keys them as, and the two have to
/// agree or `GROUP BY` and `=` disagree about the same pair.
///
/// `None` for an operand that is not a plain decimal; those still read as
/// numbers, which is a different question and a different answer.
pub fn compare(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let (a, b) = (parts(a)?, parts(b)?);
    let (a_sign, b_sign) = (sign_of(&a), sign_of(&b));
    if a_sign != b_sign {
        return Some(a_sign.cmp(&b_sign));
    }
    let (x, y) = aligned(&a, &b, a.scale.max(b.scale));
    let magnitude = cmp_magnitude(&x, &y);
    // Two negatives order by the reverse of their magnitudes: -2 is below -1.
    Some(if a_sign < 0 {
        magnitude.reverse()
    } else {
        magnitude
    })
}

/// Two magnitudes compared, ignoring the leading zeros either may carry.
fn cmp_magnitude(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let trim = |d: &[u8]| d.iter().position(|v| *v != 0).unwrap_or(d.len());
    let (a, b) = (&a[trim(a)..], &b[trim(b)..]);
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// `a + b` over two magnitudes of the same scale.
fn add_magnitude(a: &[u8], b: &[u8]) -> Vec<u8> {
    let width = a.len().max(b.len()) + 1;
    let mut sum = vec![0u8; width];
    let mut carry = 0u8;
    for i in 0..width {
        let at = |d: &[u8]| d.len().checked_sub(i + 1).map_or(0, |at| d[at]);
        let total = at(a) + at(b) + carry;
        sum[width - 1 - i] = total % 10;
        carry = total / 10;
    }
    sum
}

/// `a - b` over two magnitudes of the same scale, where `a` is the larger.
fn sub_magnitude(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut difference = vec![0u8; a.len()];
    let mut borrow = 0i8;
    for i in 0..a.len() {
        let at = |d: &[u8]| d.len().checked_sub(i + 1).map_or(0i8, |at| d[at] as i8);
        let mut total = at(a) - at(b) - borrow;
        borrow = 0;
        if total < 0 {
            total += 10;
            borrow = 1;
        }
        difference[a.len() - 1 - i] = total as u8;
    }
    difference
}

/// `a * b` over two magnitudes, long multiplication.
fn mul_magnitude(a: &[u8], b: &[u8]) -> Vec<u8> {
    // A `u32` per column rather than a `u16`, because the partial products
    // pile up before the carry runs: each is at most 81 and there is one per
    // digit of the shorter operand, so a `u16` overflows at 809 digits. MySQL
    // stops at 65, but nothing stops a *literal* being longer than that and a
    // wrapped column would be a wrong answer rather than a refusal.
    let mut product = vec![0u32; a.len() + b.len()];
    for (i, x) in a.iter().rev().enumerate() {
        for (j, y) in b.iter().rev().enumerate() {
            product[i + j] += u32::from(*x) * u32::from(*y);
        }
    }
    let mut carry = 0u32;
    for cell in &mut product {
        let total = *cell + carry;
        *cell = total % 10;
        carry = total / 10;
    }
    debug_assert_eq!(carry, 0, "the width is the sum of the two operands'");
    product.iter().rev().map(|d| *d as u8).collect()
}

/// `a / b` and `a % b` over two magnitudes, long division.
///
/// `b` must not be zero; the callers all test for that first, because a zero
/// divisor is NULL rather than an answer of any kind.
fn divmod_magnitude(a: &[u8], b: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut quotient = vec![0u8; a.len()];
    let mut remainder: Vec<u8> = Vec::with_capacity(b.len() + 1);
    for (at, digit) in a.iter().enumerate() {
        remainder.push(*digit);
        // The remainder never reaches the divisor's width plus one, so the
        // repeated subtraction below runs at most nine times per digit.
        while cmp_magnitude(&remainder, b) != std::cmp::Ordering::Less {
            remainder = sub_magnitude(&remainder, b);
            let start = remainder
                .iter()
                .position(|v| *v != 0)
                .unwrap_or(remainder.len());
            remainder.drain(..start);
            quotient[at] += 1;
        }
    }
    (quotient, remainder)
}

/// Both operands' digits at one scale, which is what every operator but `*`
/// needs before it can work on them.
fn aligned(a: &Parts, b: &Parts, scale: usize) -> (Vec<u8>, Vec<u8>) {
    debug_assert!(
        scale >= a.scale && scale >= b.scale,
        "widening only: a scale below an operand's own would drop its digits"
    );
    let widen = |p: &Parts| {
        let mut digits = p.digits.clone();
        digits.resize(digits.len() + (scale - p.scale), 0);
        digits
    };
    (widen(a), widen(b))
}

/// Digits carrying `from` places, written at `to` of them.
///
/// Rounds half away from zero, which is MySQL's rule everywhere a decimal is
/// shortened: measured on 8.3, a `DECIMAL(65,20)` times another is declared
/// with 30 places, and 5e-31 comes back 1e-30 while 4e-31 comes back zero.
fn rounded(negative: bool, digits: &[u8], from: usize, to: usize) -> String {
    if to >= from {
        return spell(negative, digits, from);
    }
    let all = padded(digits, from);
    let keep = all.len() - (from - to);
    let mut kept = all[..keep].to_vec();
    if all.get(keep).is_some_and(|d| *d >= 5) {
        kept = add_magnitude(&kept, &[1]);
    }
    spell(negative, &kept, to)
}

/// `a` at `scale`, clamped to [`MAX_SCALE`].
///
/// A product's clamp, and the one place a value can lose digits it computed
/// exactly: a `DECIMAL(65,20)` times another is declared with 30 places, not
/// 40.
fn narrowed(negative: bool, digits: &[u8], scale: usize) -> String {
    rounded(negative, digits, scale, scale.min(MAX_SCALE))
}

/// `a + b`, exactly, at the wider of the two scales.
///
/// Measured on 8.3: a `DECIMAL(10,2)` plus a `DECIMAL(8,3)`, both holding 1.5,
/// is 3.000 and is declared with three places.
pub fn add(a: &str, b: &str) -> Option<String> {
    combine(a, b, false)
}

/// `a - b`, exactly, at the wider of the two scales.
pub fn subtract(a: &str, b: &str) -> Option<String> {
    combine(a, b, true)
}

/// The sum or the difference, which differ only in the sign given to `b`.
fn combine(a: &str, b: &str, negate: bool) -> Option<String> {
    let (a, b) = (parts(a)?, parts(b)?);
    let widest = a.scale.max(b.scale);
    let (x, y) = aligned(&a, &b, widest);
    let (a_negative, b_negative) = (a.negative, b.negative != negate);
    let (negative, digits) = if a_negative == b_negative {
        (a_negative, add_magnitude(&x, &y))
    } else if cmp_magnitude(&x, &y) == std::cmp::Ordering::Less {
        (b_negative, sub_magnitude(&y, &x))
    } else {
        (a_negative, sub_magnitude(&x, &y))
    };
    Some(spell(negative, &digits, widest))
}

/// `a * b`, exactly, at the two scales added together.
///
/// Measured on 8.3: a `DECIMAL(10,2)` times a `DECIMAL(8,3)` is declared with
/// five places, so 1.5 times 1.5 is 2.25000.
pub fn multiply(a: &str, b: &str) -> Option<String> {
    let (a, b) = (parts(a)?, parts(b)?);
    let scale = a.scale + b.scale;
    Some(narrowed(
        a.negative != b.negative,
        &mul_magnitude(&a.digits, &b.digits),
        scale,
    ))
}

/// `a / b`, at the left operand's scale plus `increment`.
///
/// The increment is the session's `div_precision_increment` rather than a
/// constant, because MySQL's is: at 6, `1/3` is 0.333333 and `1.5/1.5` is
/// 1.0000000, and at 0 `1/3` is 0 — all measured on 8.3.
///
/// The rounding is not simply "round the true quotient", and the difference is
/// measurable. MySQL computes the quotient to a whole number of nine-digit
/// words — `frac1 + frac2 + div_precision_increment` rounded up to a multiple
/// of nine — truncating there, and rounds *that* to the result's scale. At an
/// increment of 0 there are no words at all over two integers, which is why
/// `7/2` is 3 there and not 4: the truncation happens before any rounding
/// could. Where the two coincide the truncation is what shows: `1.00001 / 3`
/// is 0.333336666 on 8.3, at nine places, where rounding the true quotient
/// gives 0.333336667. `1 / 7` is 0.1429 because the nine-digit intermediate
/// does get rounded down to four.
///
/// What comes back is the quotient at the scale MySQL *displays*, which is not
/// quite the number MySQL holds. Measured on 8.3, `JSON_ARRAY(1/3)` is
/// `[0.333333333]` — nine places, where the column is declared with four —
/// so the interim survives in the value and only the rendering rounds it. The
/// difference is visible wherever something reads a quotient rather than
/// printing it: `1/3 = 0.3333` is false there, `SUM(v/3)` over 32768 rows
/// totals the nine-digit values, and `(1/3)*3` is 1.0000 for 0.9999. Rounding
/// here instead is deliberate — the engine has nowhere to put a scale a value
/// does not carry, since a column is described without one at all — and it is
/// the choice that makes every *printed* quotient right. See #0118.
///
/// `None` for an operand that is not a decimal. A zero divisor is the caller's
/// to test with [`is_zero`], because its answer is NULL rather than a number.
pub fn divide(a: &str, b: &str, increment: usize) -> Option<String> {
    let (a, b) = (parts(a)?, parts(b)?);
    if b.is_zero() {
        return None;
    }
    let interim = (a.scale + b.scale + increment).div_ceil(9) * 9;
    // `a`'s digits carry a factor of 10^-a.scale and `b`'s a factor of
    // 10^-b.scale, so the shift that leaves the quotient at `interim` places
    // is `interim + b.scale - a.scale` — never negative, since `interim` is at
    // least `a.scale + b.scale + increment` rounded up.
    let mut numerator = a.digits.clone();
    numerator.resize(numerator.len() + interim + b.scale - a.scale, 0);
    let (quotient, _) = divmod_magnitude(&numerator, &b.digits);
    let scale = (a.scale + increment).min(MAX_SCALE);
    Some(rounded(a.negative != b.negative, &quotient, interim, scale))
}

/// `a % b`, at the wider of the two scales, with the dividend's sign.
///
/// Measured on 8.3: `10.5 MOD 3.00` is 1.50 — padded to the divisor's two
/// places — and `-10.5 MOD 3` and `10.5 MOD -3` are -1.5 and 1.5, so the sign
/// is the dividend's and the divisor's is ignored.
///
/// `None` for an operand that is not a decimal, or for a zero divisor.
pub fn remainder(a: &str, b: &str) -> Option<String> {
    let (a, b) = (parts(a)?, parts(b)?);
    if b.is_zero() {
        return None;
    }
    let scale = a.scale.max(b.scale);
    let (x, y) = aligned(&a, &b, scale);
    let (_, rest) = divmod_magnitude(&x, &y);
    Some(spell(a.negative, &rest, scale))
}

/// `a DIV b`, truncated toward zero, as the digits of a whole number.
///
/// Truncated rather than floored: MySQL answers -3 for `-7.5 DIV 2` where a
/// floor would say -4. The answer is a string because it is not yet known to
/// fit a BIGINT — `99999999999999999999999.0 DIV 1` is 1690 on 8.3 — and that
/// is the caller's refusal to raise.
///
/// `None` for an operand that is not a decimal, or for a zero divisor.
pub fn integer_divide(a: &str, b: &str) -> Option<String> {
    let (a, b) = (parts(a)?, parts(b)?);
    if b.is_zero() {
        return None;
    }
    let scale = a.scale.max(b.scale);
    let (x, y) = aligned(&a, &b, scale);
    let (quotient, _) = divmod_magnitude(&x, &y);
    Some(spell(a.negative != b.negative, &quotient, 0))
}

/// A number written with an exponent, spelled out in plain digits.
///
/// `'1e3'` is 1000 and `'1.5e-2'` is 0.015, exactly — the point is moved
/// through the digit string rather than through an `f64`, which is not exact
/// past about fifteen of them: `CAST('1e3' AS DECIMAL(65,30))` came back
/// 1000.000000000000089690419062898688 where MySQL answers thirty zeros.
///
/// `None` for anything that is not a number with an exponent, including a
/// plain decimal — those have their own path and do not need this one.
pub fn expanded(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let (mantissa, exponent) = trimmed.split_once(['e', 'E'])?;
    let exponent: i64 = exponent.parse().ok()?;
    let (negative, rest) = match mantissa.strip_prefix(['-', '+']) {
        Some(rest) => (mantissa.starts_with('-'), rest),
        None => (false, mantissa),
    };
    let (whole, fraction) = rest.split_once('.').unwrap_or((rest, ""));
    let plain = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if !plain(whole) || !plain(fraction) || (whole.is_empty() && fraction.is_empty()) {
        return None;
    }
    // Refused rather than expanded: an exponent of 10^18 asks for a string
    // nothing can hold, and the caller's own fallback answers that case the
    // way MySQL does.
    let digits = whole.len() + fraction.len();
    if exponent.unsigned_abs() > 1000 || digits > 1000 {
        return None;
    }

    // The point's position counted from the front of the digits, which is
    // where the exponent moves it.
    let mut all: Vec<u8> = whole.bytes().chain(fraction.bytes()).collect();
    let mut point = whole.len() as i64 + exponent;
    // Padded on whichever side the point ran off, so the digits and the point
    // stay in the same string.
    while point > all.len() as i64 {
        all.push(b'0');
    }
    while point < 0 {
        all.insert(0, b'0');
        point += 1;
    }
    let at = point as usize;
    let text = std::str::from_utf8(&all).ok()?;
    let (whole, fraction) = text.split_at(at);
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let written = if fraction.is_empty() {
        whole.to_owned()
    } else {
        format!("{whole}.{fraction}")
    };
    Some(if negative {
        format!("-{written}")
    } else {
        written
    })
}

/// A count of units at `scale`, written out.
pub fn render(units: i128, scale: usize) -> String {
    let negative = units < 0;
    let digits = units.unsigned_abs().to_string();
    let text = if scale == 0 {
        digits
    } else {
        // Left-padded so a value smaller than one unit of the whole part still
        // has something before the point: 5 units at scale 2 is `0.05`.
        let padded = format!("{digits:0>width$}", width = scale + 1);
        let split = padded.len() - scale;
        format!("{}.{}", &padded[..split], &padded[split..])
    };
    if negative {
        format!("-{text}")
    } else {
        text
    }
}

/// `text` written at exactly `scale` digits after the point.
///
/// Rounds half away from zero, which is MySQL's rule and not the one `f64`
/// printing uses: -1.005 in a `DECIMAL(10,2)` is -1.01 there, not -1.00.
pub fn rescale(text: &str, scale: usize) -> Option<String> {
    units(text, scale).map(|units| render(units, scale))
}

/// A float written at `scale` digits, for the values that are not decimals at
/// all.
///
/// MySQL reads a number out of anything and clamps it rather than refusing, so
/// this saturates instead of wrapping and answers zero for a non-finite value —
/// `'1e400'` into a `DECIMAL(6,2)` is not a number that column can hold, and
/// `Inf` is not a decimal any client can read.
/// A float's digits with no exponent, for a column described as a decimal.
///
/// All non-integer arithmetic evaluates to a float while `arithmetic_type`
/// declares the column NEWDECIMAL, so a sum over a `DECIMAL(30,2)` reaches the
/// float formatter — which reaches for an exponent past fifteen digits, as
/// MySQL does for a double. MySQL never sends a NEWDECIMAL that way, and a
/// driver handing the text to a decimal parser has no reason to accept `4e16`.
/// The scale is still the f64's rather than the one MySQL computes; that is
/// #0070, and this only keeps the form readable.
pub fn plain(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_owned();
    }
    // Rust's `Display` for an f64 is always positional, however many digits
    // that takes, which is exactly what is wanted here.
    let text = value.to_string();
    text.strip_suffix(".0").map(str::to_owned).unwrap_or(text)
}

pub fn from_f64(value: f64, scale: usize) -> String {
    if !value.is_finite() {
        return render(0, scale);
    }
    let scaled = value * 10f64.powi(scale as i32);
    let units = if scaled >= i128::MAX as f64 {
        i128::MAX
    } else if scaled <= i128::MIN as f64 {
        i128::MIN
    } else {
        scaled.round() as i128
    };
    render(units, scale)
}

/// The same number with as few digits after the point as spell it.
///
/// `2.50` at scale two is `2.5` at scale one and `2` at scale zero is the same
/// number as `2.00`, which is what lets one key stand for every spelling.
///
/// Worked on the characters rather than counted in an `i128`, because the
/// counted form stops at 38 digits and arithmetic now reaches past that: a
/// product of two 35-digit operands is 70 digits and MySQL prints all of them.
/// Keyed by its raw text at that width, `big * big` and `big * big * 1.0`
/// were two groups for one number, so a `UNION` over them stopped
/// de-duplicating.
///
/// `None` for text that is not a decimal, which has no number to shorten.
pub fn shortest(text: &str) -> Option<String> {
    let mut value = parts(text)?;
    while value.scale > 0 && value.digits.last() == Some(&0) {
        value.digits.pop();
        value.scale -= 1;
    }
    Some(spell(value.negative, &value.digits, value.scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scale rules, measured on 8.3 over a `DECIMAL(10,2)` and a
    /// `DECIMAL(8,3)` both holding 1.5. Every one of these was read off the
    /// server, and the table in the issue was checked against it rather than
    /// trusted.
    #[test]
    fn the_operators_carry_mysql_s_scales() {
        assert_eq!(add("1.50", "1.500").as_deref(), Some("3.000"));
        assert_eq!(add("1.500", "1.50").as_deref(), Some("3.000"));
        assert_eq!(subtract("1.50", "1.500").as_deref(), Some("0.000"));
        assert_eq!(subtract("1.500", "1.50").as_deref(), Some("0.000"));
        assert_eq!(multiply("1.50", "1.500").as_deref(), Some("2.25000"));
        assert_eq!(multiply("1.500", "1.50").as_deref(), Some("2.25000"));
        assert_eq!(
            divide("1.50", "1.500", DEFAULT_DIV_PRECISION_INCREMENT).as_deref(),
            Some("1.000000")
        );
        assert_eq!(
            divide("1.500", "1.50", DEFAULT_DIV_PRECISION_INCREMENT).as_deref(),
            Some("1.0000000")
        );
        assert_eq!(remainder("1.50", "1.500").as_deref(), Some("0.000"));
        assert_eq!(integer_divide("1.50", "1.500").as_deref(), Some("1"));
        // And against an integer, whose scale is zero.
        assert_eq!(add("1.50", "1").as_deref(), Some("2.50"));
        assert_eq!(multiply("1.50", "2").as_deref(), Some("3.00"));
        assert_eq!(add("1.50", "0.5").as_deref(), Some("2.00"));
    }

    /// A sign says nothing about how many places a number is written to, and
    /// neither operand's sign reaches the other's scale.
    #[test]
    fn a_sign_keeps_the_scale_and_the_dividend_keeps_its_own() {
        assert_eq!(subtract("0", "1.50").as_deref(), Some("-1.50"));
        assert_eq!(subtract("0", "-1.50").as_deref(), Some("1.50"));
        assert_eq!(absolute("-1.50").as_deref(), Some("1.50"));
        assert_eq!(absolute("-0.00").as_deref(), Some("0.00"));
        // Measured: the remainder takes the *dividend's* sign, and the
        // divisor's is ignored entirely.
        assert_eq!(remainder("10.5", "3").as_deref(), Some("1.5"));
        assert_eq!(remainder("-10.5", "3").as_deref(), Some("-1.5"));
        assert_eq!(remainder("10.5", "-3").as_deref(), Some("1.5"));
        assert_eq!(remainder("-10.5", "-3").as_deref(), Some("-1.5"));
        // And it pads to the wider scale rather than to its own digits.
        assert_eq!(remainder("10.5", "3.00").as_deref(), Some("1.50"));
        assert_eq!(remainder("3.00", "1").as_deref(), Some("0.00"));
        // `DIV` truncates toward zero rather than flooring.
        assert_eq!(integer_divide("-7.5", "2").as_deref(), Some("-3"));
        assert_eq!(integer_divide("7.5", "-2").as_deref(), Some("-3"));
        assert_eq!(integer_divide("1.5", "0.4").as_deref(), Some("3"));
    }

    /// Division is not "round the true quotient". MySQL computes it to a whole
    /// number of nine-digit words and truncates there, then rounds *that* to
    /// the result's scale — so where the two coincide the truncation shows.
    #[test]
    fn division_truncates_at_nine_digit_words_before_it_rounds() {
        // 0.33333666666… truncated at nine places, not rounded to …667.
        assert_eq!(
            divide("1.00001", "3", DEFAULT_DIV_PRECISION_INCREMENT).as_deref(),
            Some("0.333336666")
        );
        // And where there are digits to spare it does round: 1/7 at nine
        // places is 0.142857142, rounded to four.
        assert_eq!(
            divide("1", "7", DEFAULT_DIV_PRECISION_INCREMENT).as_deref(),
            Some("0.1429")
        );
        assert_eq!(
            divide("2", "3", DEFAULT_DIV_PRECISION_INCREMENT).as_deref(),
            Some("0.6667")
        );
        assert_eq!(
            divide("-2", "3", DEFAULT_DIV_PRECISION_INCREMENT).as_deref(),
            Some("-0.6667")
        );
        // 25 places on the left asks for 29 and computes 36, which is where
        // the rounding at the far end is visible.
        assert_eq!(
            divide(
                "1.0000000000000000000000000",
                "7",
                DEFAULT_DIV_PRECISION_INCREMENT
            )
            .as_deref(),
            Some("0.14285714285714285714285714286")
        );
        assert_eq!(
            divide("1", "3", DEFAULT_DIV_PRECISION_INCREMENT).as_deref(),
            Some("0.3333")
        );
        assert_eq!(
            divide("10", "3", DEFAULT_DIV_PRECISION_INCREMENT).as_deref(),
            Some("3.3333")
        );
        // A zero divisor has no answer here; NULL is the caller's to give.
        assert_eq!(divide("1", "0", DEFAULT_DIV_PRECISION_INCREMENT), None);
        assert_eq!(remainder("1", "0.00"), None);
        assert_eq!(integer_divide("1", "0"), None);
        assert!(is_zero("0.00") && is_zero("-0") && !is_zero("0.01"));
    }

    /// Past what an `i128` counts, which is where the `f64` fallback used to
    /// take over and lose the scale along with the digits.
    #[test]
    fn the_operators_reach_past_an_i128() {
        let big = "99999999999999999999999999999999999";
        // 70 digits: MySQL prints it in full under a column it still
        // describes `decimal(65,0)`, so the precision is not clamped.
        assert_eq!(
            multiply(big, big).as_deref(),
            Some("9999999999999999999999999999999999800000000000000000000000000000000001")
        );
        // A `DECIMAL(30,10)` times a `DECIMAL(65,30)`: 40 places asked for,
        // 30 given, and the digits kept exactly.
        assert_eq!(
            multiply("3.5000000000", "1.500000000000000000000000000000").as_deref(),
            Some("5.250000000000000000000000000000")
        );
        assert_eq!(
            add(
                "1.500000000000000000000000000000",
                "1.500000000000000000000000000000"
            )
            .as_deref(),
            Some("3.000000000000000000000000000000")
        );
        // Only `*` and `/` clamp, and they round half away from zero doing it:
        // 5e-31 is 1e-30 and 4e-31 is nothing.
        let tiny = |digits: &str| multiply(digits, "0.00000000000000000001");
        assert_eq!(
            tiny("0.00000000005").as_deref(),
            Some("0.000000000000000000000000000001")
        );
        assert_eq!(
            tiny("0.00000000004").as_deref(),
            Some("0.000000000000000000000000000000")
        );
        assert_eq!(
            tiny("-0.00000000005").as_deref(),
            Some("-0.000000000000000000000000000001")
        );
        // 37 places, which only a literal reaches: `+`, `-` and `%` keep all
        // of them where `*` and `/` do not.
        let long = "1.5000000000000000000000000000000000000";
        assert_eq!(add(long, "0").as_deref(), Some(long));
        assert_eq!(subtract(long, "0").as_deref(), Some(long));
        assert_eq!(
            remainder(long, "1").as_deref(),
            Some("0.5000000000000000000000000000000000000")
        );
        assert_eq!(
            multiply(long, "1").as_deref(),
            Some("1.500000000000000000000000000000")
        );
        assert_eq!(
            divide(long, "1", DEFAULT_DIV_PRECISION_INCREMENT).as_deref(),
            Some("1.500000000000000000000000000000")
        );
    }

    /// The digit-string operators against the counted ones, on the values both
    /// can hold. The same argument `the_wide_rescaler_agrees_with_the_counted_one`
    /// makes: two ways to compute one answer is two answers waiting to drift,
    /// and the aggregates still fold in an `i128`.
    #[test]
    fn the_wide_operators_agree_with_a_counted_reference() {
        let values = [
            "0",
            "1",
            "-1",
            "1.5",
            "-1.5",
            "2.25",
            "-2.25",
            "0.001",
            "-0.001",
            "10.5",
            "-10.5",
            "100",
            "-100",
            "0.5",
            "7",
            "-7",
            "12345.6789",
        ];
        for a in values {
            for b in values {
                let scale = scale_of(a).max(scale_of(b));
                let (x, y) = (units(a, scale).unwrap(), units(b, scale).unwrap());
                assert_eq!(
                    add(a, b).as_deref(),
                    Some(render(x + y, scale).as_str()),
                    "{a} + {b}"
                );
                assert_eq!(
                    subtract(a, b).as_deref(),
                    Some(render(x - y, scale).as_str()),
                    "{a} - {b}"
                );
                let product = scale_of(a) + scale_of(b);
                let counted = units(a, scale_of(a)).unwrap() * units(b, scale_of(b)).unwrap();
                assert_eq!(
                    multiply(a, b).as_deref(),
                    Some(render(counted, product).as_str()),
                    "{a} * {b}"
                );
                if y == 0 {
                    assert_eq!(remainder(a, b), None, "{a} % {b}");
                    continue;
                }
                assert_eq!(
                    remainder(a, b).as_deref(),
                    Some(render(x % y, scale).as_str()),
                    "{a} % {b}"
                );
                assert_eq!(
                    integer_divide(a, b).as_deref(),
                    Some(render(x / y, 0).as_str()),
                    "{a} DIV {b}"
                );
            }
        }
    }

    /// A decimal is written the one way however it was reached: nothing before
    /// the point is still a zero, a value that came out zero has no sign, and
    /// a remainder short of the scale is padded rather than truncated.
    #[test]
    fn an_answer_is_spelled_the_way_mysql_spells_it() {
        assert_eq!(add("0.04", "0.01").as_deref(), Some("0.05"));
        assert_eq!(subtract("0.01", "0.02").as_deref(), Some("-0.01"));
        assert_eq!(multiply("-0.5", "0").as_deref(), Some("0.0"));
        assert_eq!(subtract("0.5", "0.5").as_deref(), Some("0.0"));
        assert_eq!(remainder("1.500", "1.50").as_deref(), Some("0.000"));
        assert_eq!(add("007.5", "0").as_deref(), Some("7.5"));
        assert_eq!(add(".5", "0").as_deref(), Some("0.5"));
        // Text that is not a decimal is not this path's to answer.
        assert_eq!(add("1e3", "1"), None);
        assert_eq!(multiply("abc", "1"), None);
        assert_eq!(divide("1", "-", DEFAULT_DIV_PRECISION_INCREMENT), None);
    }

    /// Comparison against the counted reference, and past where the counted
    /// one can reach. The `f64` this replaced gave up at seventeen significant
    /// digits, which is inside the range storage and arithmetic are exact
    /// over — so one row could say `a - b` is 1 and `a = b` is true.
    #[test]
    fn comparison_is_exact_at_every_width() {
        use std::cmp::Ordering;
        let values = [
            "0", "-0.00", "1", "-1", "1.5", "-1.5", "1.50", "2.5", "-2.5", "10.00", "0.001",
            "-0.001", "100", "-100",
        ];
        for a in values {
            for b in values {
                let scale = scale_of(a).max(scale_of(b));
                let (x, y) = (units(a, scale).unwrap(), units(b, scale).unwrap());
                assert_eq!(compare(a, b), Some(x.cmp(&y)), "{a} <=> {b}");
            }
        }
        // 38 digits one apart: an `f64` calls these equal and an `i128` counts
        // them only just, which is why the digits are compared instead.
        let high = "10000000000000000000000000000000000001";
        let low = "10000000000000000000000000000000000000";
        assert_eq!(compare(high, low), Some(Ordering::Greater));
        assert_eq!(compare(low, high), Some(Ordering::Less));
        assert_eq!(compare(high, high), Some(Ordering::Equal));
        // And the same pair negated, where the magnitudes order the other way.
        let (nhigh, nlow) = (format!("-{high}"), format!("-{low}"));
        assert_eq!(compare(&nhigh, &nlow), Some(Ordering::Less));
        assert_eq!(compare(&nlow, &nhigh), Some(Ordering::Greater));
        assert_eq!(compare(&nhigh, low), Some(Ordering::Less));
        // 70 digits, which is what a product of two `DECIMAL(65,0)` reaches.
        let wide = multiply(high, high).unwrap();
        assert_eq!(compare(&wide, high), Some(Ordering::Greater));
        assert_eq!(compare(&wide, &wide), Some(Ordering::Equal));
        // A zero has no sign however it is spelled, which is what `shortest`
        // keys it as too — the two have to agree or `GROUP BY` and `=` do not.
        assert_eq!(compare("-0.00", "0"), Some(Ordering::Equal));
        assert_eq!(shortest("-0.00").as_deref(), shortest("0").as_deref());
        // Not this path's question.
        assert_eq!(compare("1e3", "1000"), None);
        assert_eq!(compare("1", "abc"), None);
    }

    /// The two rescalers have to be one answer, because `decimal_digits` picks
    /// between them by whether an `i128` happened to overflow — a boundary no
    /// caller can see and none should be able to feel.
    #[test]
    fn the_wide_rescaler_agrees_with_the_counted_one() {
        let values = [
            "0",
            "1",
            "1.5",
            "2.5",
            "-2.5",
            "1.005",
            "-1.005",
            "0.4",
            "-0.4",
            "9.99",
            "-9.99",
            "0.05",
            "-0.05",
            "12345.6789",
            "-12345.6789",
            "1.0049",
            "999.995",
            "-999.995",
            "+7.5",
            "  7.5  ",
            ".5",
            "-.5",
            "100",
            "0.000",
        ];
        for text in values {
            for scale in 0..=6 {
                assert_eq!(
                    rescale(text, scale),
                    rescale_wide(text, scale),
                    "{text} at scale {scale}"
                );
            }
        }
    }

    /// And it answers where the counted one cannot, which is the whole reason
    /// it exists: a `DECIMAL(40,30)` overflows an `i128` at ten integer digits
    /// and MySQL holds it, so the fallback is reached by ordinary values.
    #[test]
    fn the_wide_rescaler_reaches_past_an_i128() {
        assert_eq!(rescale("1234567890.5", 30), None);
        assert_eq!(
            rescale_wide("1234567890.5", 30).as_deref(),
            Some("1234567890.500000000000000000000000000000")
        );
        // The carry runs off the front of the number, which is why it is
        // propagated through the digits rather than added to them.
        assert_eq!(
            rescale_wide("9.9999999999999999999999999999995", 30).as_deref(),
            Some("10.000000000000000000000000000000")
        );
        // 42 digits, which MySQL's 65 hold and an `i128`'s 38 do not.
        let long = "123456789012345678901234567890123456789012";
        assert_eq!(rescale(long, 0), None);
        assert_eq!(rescale_wide(long, 0).as_deref(), Some(long));
        // Text that is not a decimal is not this function's to answer.
        assert_eq!(rescale_wide("abc", 2), None);
        assert_eq!(rescale_wide("1e3", 2), None);
        assert_eq!(rescale_wide("-", 2), None);
        assert_eq!(rescale_wide(".", 2), None);
    }

    /// #0116. The sign comes off the digits, and zero has none however it was
    /// spelled.
    ///
    /// Asserted here rather than through `SELECT SIGN(...)` because this is
    /// the level the two readings differ at. Every decimal MySQL can hold —
    /// 65 digits, 30 of them after the point — has a magnitude an `f64`
    /// represents, so over those a float reading gives the same answer and a
    /// SQL test would pass with this function gone. It is the decimals this
    /// engine holds and MySQL does not that separate them: a literal keeps all
    /// of its places here where 8.3 cuts it at 72, so a value below the
    /// smallest `f64` subnormal is reachable, and read through a float it
    /// loses its sign along with everything else.
    #[test]
    fn the_sign_of_a_decimal_is_read_off_its_digits() {
        assert_eq!(sign("1.50"), Some(1));
        assert_eq!(sign("-1.50"), Some(-1));
        assert_eq!(sign("+1.50"), Some(1));
        assert_eq!(sign("0"), Some(0));
        assert_eq!(sign("0.00"), Some(0));
        // A signed zero is a zero. `f64::signum` splits these two.
        assert_eq!(sign("-0.00"), Some(0));
        assert_eq!(sign("+0.00"), Some(0));
        // Past what an `f64` can hold in either direction.
        let tiny = format!("0.{}1", "0".repeat(400));
        assert_eq!(
            tiny.parse::<f64>().unwrap().partial_cmp(&0.0),
            Some(std::cmp::Ordering::Equal)
        );
        assert_eq!(sign(&tiny), Some(1));
        assert_eq!(sign(&format!("-{tiny}")), Some(-1));
        let huge = "9".repeat(400);
        assert_eq!(sign(&huge), Some(1));
        assert_eq!(sign(&format!("-{huge}")), Some(-1));
        // Not a plain decimal, so not this function's to answer: an exponent
        // is a double to MySQL and text is not a number at all.
        assert_eq!(sign("1e3"), None);
        assert_eq!(sign("abc"), None);
        assert_eq!(sign(""), None);
    }
}
