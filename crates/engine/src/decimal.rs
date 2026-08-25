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
//! the last place, which is exact for anything an `i128` holds and keeps the
//! scale explicit rather than implied by whatever a float printed.

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

    let (whole, fraction) = digits.split_at(digits.len() - scale);
    let whole = std::str::from_utf8(whole).ok()?.trim_start_matches('0');
    let fraction = std::str::from_utf8(fraction).ok()?;
    // Never empty before the point: `render` left-pads for the same reason,
    // because five units at scale two is `0.05` and not `.05`.
    let whole = if whole.is_empty() { "0" } else { whole };
    let written = if scale == 0 {
        whole.to_owned()
    } else {
        format!("{whole}.{fraction}")
    };
    // A value that rounded to zero has no sign, which is `render`'s answer
    // too: -0.4 at scale zero is `0` and not `-0`.
    let zero = whole == "0" && fraction.bytes().all(|b| b == b'0');
    Some(if negative && !zero {
        format!("-{written}")
    } else {
        written
    })
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

/// `numerator / denominator`, rounded half away from zero.
///
/// The division an average is: MySQL rounds the result at its own scale rather
/// than truncating, so 2.99 over three rows is 0.996667 and not 0.996666.
pub fn divide(numerator: i128, denominator: i128) -> Option<i128> {
    if denominator == 0 {
        return None;
    }
    let (n, d) = (numerator, denominator);
    let quotient = n / d;
    // Unsigned magnitudes, so neither the doubling nor the absolute value can
    // overflow — `i128::MIN.abs()` panics, and this is a public helper that
    // does not get to assume its caller passed a row count.
    let remainder = (n % d).unsigned_abs();
    if remainder.saturating_mul(2) >= d.unsigned_abs() {
        let away = if (n < 0) != (d < 0) { -1 } else { 1 };
        quotient.checked_add(away)
    } else {
        Some(quotient)
    }
}

/// The extra digits MySQL gives an average over the scale it averaged.
///
/// Four, from `div_precision_increment`, which is the documented default:
/// `AVG` of a `DECIMAL(10,2)` is a `DECIMAL(_,6)` and `AVG` of an integer is a
/// `DECIMAL(_,4)`.
pub const AVERAGE_EXTRA_SCALE: usize = 4;

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
/// number as `2.00`, which is what lets one key stand for every spelling. The
/// scale comes back with the units: dropping it would make `2.50` and `25.00`
/// the same 25.
pub fn shortest(mut units: i128, mut scale: usize) -> (i128, usize) {
    while scale > 0 && units % 10 == 0 {
        units /= 10;
        scale -= 1;
    }
    (units, scale)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
