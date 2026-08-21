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
