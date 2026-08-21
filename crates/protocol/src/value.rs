//! Column values, and their encoding in both the text and binary protocols.
//!
//! The two protocols disagree about almost everything: the text protocol sends
//! every value as a length-encoded string, while the binary protocol used by
//! prepared statements sends fixed-width little-endian representations and
//! carries NULLs in a bitmap ahead of the row. Both live here so a value has
//! exactly one definition.

use crate::buf::{BufReader, BufResult, BufWriter, Truncated};
use crate::column::ColumnType;

/// A single column value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    I64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
    /// Text already in its MySQL string representation. Also carries DECIMAL,
    /// DATE, DATETIME and TIME, which MySQL transmits as strings in the text
    /// protocol.
    Str(String),
    Bytes(Vec<u8>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The MySQL type code this value presents as.
    pub fn inferred_type(&self) -> ColumnType {
        match self {
            Value::Null => ColumnType::Null,
            Value::I64(_) | Value::U64(_) => ColumnType::LongLong,
            Value::F32(_) => ColumnType::Float,
            Value::F64(_) => ColumnType::Double,
            Value::Str(_) => ColumnType::VarString,
            Value::Bytes(_) => ColumnType::Blob,
        }
    }

    /// The text-protocol form: everything becomes a string.
    pub fn encode_text(&self, w: &mut BufWriter) {
        match self {
            // 0xFB is the text protocol's NULL marker.
            Value::Null => {
                w.u8(0xfb);
            }
            Value::I64(v) => {
                w.lenenc_str(&v.to_string());
            }
            Value::U64(v) => {
                w.lenenc_str(&v.to_string());
            }
            // Six significant digits, which is what MySQL shows for a FLOAT
            // and is the reason the value is kept at four bytes at all: the
            // widened double reads back 1.2345678806304932 for a column MySQL
            // prints as 1.23457.
            Value::F32(v) => {
                w.lenenc_str(&format_float(*v as f64, Some(FLOAT_DIGITS)));
            }
            Value::F64(v) => {
                w.lenenc_str(&format_f64(*v));
            }
            Value::Str(v) => {
                w.lenenc_str(v);
            }
            Value::Bytes(v) => {
                w.lenenc_bytes(v);
            }
        }
    }

    /// The binary-protocol form. NULLs are not written here at all; they are
    /// signalled by the row's null bitmap.
    ///
    /// The width comes from the column's declared type, never from the value.
    /// A handler may legitimately declare `ColumnType::Long` and hand back a
    /// `Value::I64`; if that were written as 8 bytes the client would read 4
    /// and every subsequent column in the result set would be garbage, with no
    /// error raised anywhere.
    pub fn encode_binary(&self, w: &mut BufWriter, ty: ColumnType) {
        if self.is_null() {
            return;
        }

        match ty {
            ColumnType::Tiny => {
                w.u8(self.as_i64().unwrap_or(0) as u8);
            }
            ColumnType::Short | ColumnType::Year => {
                w.u16(self.as_i64().unwrap_or(0) as u16);
            }
            ColumnType::Long | ColumnType::Int24 => {
                w.u32(self.as_i64().unwrap_or(0) as u32);
            }
            ColumnType::LongLong => {
                w.u64(self.as_i64().unwrap_or(0) as u64);
            }
            ColumnType::Float => {
                w.f32(self.as_f64().unwrap_or(0.0) as f32);
            }
            ColumnType::Double => {
                w.f64(self.as_f64().unwrap_or(0.0));
            }
            // Temporal types have a packed binary form. Sending the text
            // form instead makes a client read the string's length byte as a
            // year: MySQL's own drivers panic rather than error on that.
            ColumnType::Date | ColumnType::DateTime | ColumnType::Timestamp => {
                encode_binary_datetime(w, &self.rendered());
            }
            ColumnType::Time => {
                encode_binary_time(w, &self.rendered());
            }
            // Everything else — strings, decimals, JSON, blobs — is
            // length-encoded in the binary protocol just as in the text one.
            _ => match self {
                Value::Bytes(v) => {
                    w.lenenc_bytes(v);
                }
                Value::Str(v) => {
                    w.lenenc_str(v);
                }
                other => {
                    let mut text = BufWriter::new();
                    other.encode_text(&mut text);
                    w.bytes(text.as_slice());
                }
            },
        }
    }

    /// The value's text form, used where a type needs re-parsing.
    fn rendered(&self) -> String {
        match self {
            Value::Str(v) => v.clone(),
            Value::Bytes(v) => String::from_utf8_lossy(v).into_owned(),
            other => {
                let mut w = BufWriter::new();
                other.encode_text(&mut w);
                // encode_text writes a length-encoded string; the digits start
                // after the single-byte length for anything this short.
                String::from_utf8_lossy(&w.into_inner()[1..]).into_owned()
            }
        }
    }

    /// The value as a signed integer, where that is meaningful.
    fn as_i64(&self) -> Option<i64> {
        match self {
            Value::I64(v) => Some(*v),
            Value::U64(v) => Some(*v as i64),
            Value::F32(v) => Some(*v as i64),
            Value::F64(v) => Some(*v as i64),
            Value::Str(v) => v.trim().parse().ok(),
            _ => None,
        }
    }

    /// The value as a float, where that is meaningful.
    fn as_f64(&self) -> Option<f64> {
        match self {
            Value::I64(v) => Some(*v as f64),
            Value::U64(v) => Some(*v as f64),
            Value::F32(v) => Some(*v as f64),
            Value::F64(v) => Some(*v),
            Value::Str(v) => v.trim().parse().ok(),
            _ => None,
        }
    }

    /// Decodes a bound parameter sent by `COM_STMT_EXECUTE`.
    pub fn decode_binary(r: &mut BufReader<'_>, ty: u8, unsigned: bool) -> BufResult<Value> {
        Ok(match ty {
            // MYSQL_TYPE_NULL
            0x06 => Value::Null,
            // TINY
            0x01 => {
                let v = r.u8()?;
                if unsigned {
                    Value::U64(v as u64)
                } else {
                    Value::I64(v as i8 as i64)
                }
            }
            // SHORT | YEAR
            0x02 | 0x0d => {
                let v = r.u16()?;
                if unsigned {
                    Value::U64(v as u64)
                } else {
                    Value::I64(v as i16 as i64)
                }
            }
            // LONG | INT24
            0x03 | 0x09 => {
                let v = r.u32()?;
                if unsigned {
                    Value::U64(v as u64)
                } else {
                    Value::I64(v as i32 as i64)
                }
            }
            // LONGLONG
            0x08 => {
                let v = r.u64()?;
                if unsigned {
                    Value::U64(v)
                } else {
                    Value::I64(v as i64)
                }
            }
            // FLOAT
            0x04 => Value::F32(r.f32()?),
            // DOUBLE
            0x05 => Value::F64(r.f64()?),
            // DATE | DATETIME | TIMESTAMP
            0x0a | 0x0c | 0x07 => decode_binary_datetime(r)?,
            // TIME
            0x0b => decode_binary_time(r)?,
            // Everything else arrives as a length-encoded byte string.
            _ => match r.lenenc_bytes()? {
                None => Value::Null,
                Some(bytes) => match std::str::from_utf8(bytes) {
                    Ok(s) => Value::Str(s.to_owned()),
                    Err(_) => Value::Bytes(bytes.to_vec()),
                },
            },
        })
    }
}

/// MySQL renders floats without a trailing `.0` and without an exponent for
/// ordinary magnitudes; `{}` on an f64 is close enough for the values a test
/// suite compares.
fn format_f64(v: f64) -> String {
    format_float(v, None)
}

/// How many significant digits MySQL shows for a four-byte float.
///
/// `FLT_DIG`, the same constant its own printer uses.
pub const FLOAT_DIGITS: usize = 6;

/// A float as MySQL prints one, at `digits` significant digits.
///
/// `None` for a DOUBLE, whose digits are however many it takes to read the
/// value back — `Some(6)` for a FLOAT, which MySQL prints at `FLT_DIG` and so
/// shows `1.23457` for a column holding 1.2345678806304932.
///
/// The choice between the two forms is `my_gcvt`'s, the same one a document's
/// numbers go through:
///
/// ```text
/// decpt >= -14 && (decpt <= 15 || len > decpt)
/// ```
///
/// `decpt` is where the point falls, one past the exponent, and `len` is how
/// many digits are being written. Rust's `Display` never picks the second form
/// at all, so `1e20` came out as twenty-one digits where MySQL writes `1e20`.
/// Unlike a document, a column drops a trailing `.0`: `-2.0` prints as `-2`.
pub fn format_float(v: f64, digits: Option<usize>) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v.is_sign_negative() {
            "-Inf".to_owned()
        } else {
            "Inf".to_owned()
        };
    }

    // Rounded first, so a FLOAT is measured and written at the width MySQL
    // shows rather than at the width its double happens to need. Back through
    // a parse, because the rounding changes which value is being printed:
    // 16777216 at six digits *is* 16777200.
    let (value, scientific) = match digits {
        None => (v, format!("{v:e}")),
        Some(digits) => {
            let rounded = format!("{v:.*e}", digits.saturating_sub(1));
            let value: f64 = rounded.parse().unwrap_or(v);
            (value, format!("{value:e}"))
        }
    };

    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let decpt = exponent + 1;
    let len = mantissa.chars().filter(char::is_ascii_digit).count() as i32;

    if decpt < -14 || (decpt > 15 && len <= decpt) {
        return scientific;
    }
    let fixed = value.to_string();
    fixed.strip_suffix(".0").map(str::to_owned).unwrap_or(fixed)
}

/// The binary DATE/DATETIME encoding: a length byte, then progressively more
/// fields.
///
/// Only 0, 4, 7 and 11 are legal lengths. Anything else is rejected rather than
/// falling through to the widest layout: consuming 11 bytes for a field the
/// client declared as 5 would desynchronise every parameter after it, turning
/// one malformed field into a silently wrong statement.
fn decode_binary_datetime(r: &mut BufReader<'_>) -> BufResult<Value> {
    let len = r.u8()?;
    if len == 0 {
        return Ok(Value::Str("0000-00-00".to_owned()));
    }
    if !matches!(len, 4 | 7 | 11) {
        return Err(Truncated);
    }

    let year = r.u16()?;
    let month = r.u8()?;
    let day = r.u8()?;

    if len == 4 {
        return Ok(Value::Str(format!("{year:04}-{month:02}-{day:02}")));
    }

    let hour = r.u8()?;
    let minute = r.u8()?;
    let second = r.u8()?;

    if len == 7 {
        return Ok(Value::Str(format!(
            "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
        )));
    }

    let micros = r.u32()?;
    Ok(Value::Str(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{micros:06}"
    )))
}

/// The binary TIME encoding, which carries a sign and a day count because
/// MySQL TIME spans -838:59:59 to 838:59:59.
///
/// The day count arrives as an untrusted `u32`, so the hour total is computed
/// in `u64` and range-checked. Computing it in `u32` overflows and panics for
/// any `days` above 178_956_970, which a client can simply send.
fn decode_binary_time(r: &mut BufReader<'_>) -> BufResult<Value> {
    /// The largest hour value MySQL's TIME type can represent.
    const MAX_TIME_HOURS: u64 = 838;

    let len = r.u8()?;
    if len == 0 {
        return Ok(Value::Str("00:00:00".to_owned()));
    }
    if !matches!(len, 8 | 12) {
        return Err(Truncated);
    }

    let negative = r.u8()? == 1;
    let days = r.u32()? as u64;
    let hours = r.u8()? as u64;
    let minutes = r.u8()?;
    let seconds = r.u8()?;

    let total_hours = days.saturating_mul(24).saturating_add(hours);
    if total_hours > MAX_TIME_HOURS {
        return Err(Truncated);
    }
    let sign = if negative { "-" } else { "" };

    if len == 8 {
        return Ok(Value::Str(format!(
            "{sign}{total_hours:02}:{minutes:02}:{seconds:02}"
        )));
    }

    let micros = r.u32()?;
    Ok(Value::Str(format!(
        "{sign}{total_hours:02}:{minutes:02}:{seconds:02}.{micros:06}"
    )))
}

/// Builds the null bitmap that precedes a binary result row.
///
/// Result rows carry a two-bit offset before the first column; bound
/// parameters do not. That offset is the single most common bug in binary
/// protocol implementations, so it is an explicit argument.
pub fn null_bitmap(values: &[Value], offset: usize) -> Vec<u8> {
    let mut bitmap = vec![0u8; (values.len() + offset).div_ceil(8)];
    for (i, value) in values.iter().enumerate() {
        if value.is_null() {
            let pos = i + offset;
            bitmap[pos / 8] |= 1 << (pos % 8);
        }
    }
    bitmap
}

/// Reads bit `index` of a null bitmap.
pub fn bitmap_is_null(bitmap: &[u8], index: usize) -> bool {
    bitmap
        .get(index / 8)
        .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
}

/// Parses a fractional-seconds field into whole microseconds.
///
/// The digits are counted in characters, not bytes: taking six `chars` and then
/// measuring `len()` overflows the scale subtraction the moment the fraction
/// contains a multi-byte character, which a client can put there by storing
/// junk in a DATETIME column.
fn parse_fraction(fraction: &str) -> u32 {
    let digits: String = fraction
        .chars()
        .filter(char::is_ascii_digit)
        .take(6)
        .collect();
    if digits.is_empty() {
        return 0;
    }
    // A shorter fraction is scaled up rather than truncated: `.5` is 500000µs.
    let scale = 10u32.pow(6 - digits.chars().count() as u32);
    digits.parse::<u32>().unwrap_or(0) * scale
}

/// Splits a `YYYY-MM-DD[ HH:MM:SS[.ffffff]]` string into its parts.
fn datetime_parts(text: &str) -> Option<(u16, u8, u8, u8, u8, u8, u32)> {
    let text = text.trim();
    let (date, time) = match text.split_once([' ', 'T']) {
        Some((d, t)) => (d, Some(t)),
        None => (text, None),
    };

    let mut date_parts = date.split('-');
    let year: u16 = date_parts.next()?.parse().ok()?;
    let month: u8 = date_parts.next().unwrap_or("1").parse().unwrap_or(1);
    let day: u8 = date_parts.next().unwrap_or("1").parse().unwrap_or(1);

    let (mut hour, mut minute, mut second, mut micros) = (0u8, 0u8, 0u8, 0u32);
    if let Some(time) = time {
        let time = time.trim_end_matches('Z');
        let (clock, fraction) = match time.split_once('.') {
            Some((c, f)) => (c, Some(f)),
            None => (time, None),
        };
        let mut clock_parts = clock.split(':');
        hour = clock_parts.next().unwrap_or("0").parse().unwrap_or(0);
        minute = clock_parts.next().unwrap_or("0").parse().unwrap_or(0);
        second = clock_parts.next().unwrap_or("0").parse().unwrap_or(0);
        if let Some(fraction) = fraction {
            micros = parse_fraction(fraction);
        }
    }

    Some((year, month, day, hour, minute, second, micros))
}

/// Writes the packed binary DATE/DATETIME form.
fn encode_binary_datetime(w: &mut BufWriter, text: &str) {
    let Some((year, month, day, hour, minute, second, micros)) = datetime_parts(text) else {
        // A value that cannot be parsed is sent as the zero date rather than a
        // malformed packet that would desynchronise the connection.
        w.u8(0);
        return;
    };

    if micros > 0 {
        w.u8(11)
            .u16(year)
            .u8(month)
            .u8(day)
            .u8(hour)
            .u8(minute)
            .u8(second)
            .u32(micros);
    } else if hour != 0 || minute != 0 || second != 0 {
        w.u8(7)
            .u16(year)
            .u8(month)
            .u8(day)
            .u8(hour)
            .u8(minute)
            .u8(second);
    } else {
        w.u8(4).u16(year).u8(month).u8(day);
    }
}

/// Writes the packed binary TIME form.
fn encode_binary_time(w: &mut BufWriter, text: &str) {
    let text = text.trim();
    let negative = text.starts_with('-');
    let body = text.trim_start_matches('-');

    let (clock, fraction) = match body.split_once('.') {
        Some((c, f)) => (c, Some(f)),
        None => (body, None),
    };
    let mut parts = clock.split(':');
    let total_hours: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let minutes: u8 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let seconds: u8 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let micros: u32 = fraction.map(parse_fraction).unwrap_or(0);

    // MySQL splits the hour count into whole days plus a remainder.
    let days = total_hours / 24;
    let hours = (total_hours % 24) as u8;

    if micros > 0 {
        w.u8(12)
            .u8(u8::from(negative))
            .u32(days)
            .u8(hours)
            .u8(minutes)
            .u8(seconds)
            .u32(micros);
    } else if days == 0 && hours == 0 && minutes == 0 && seconds == 0 {
        w.u8(0);
    } else {
        w.u8(8)
            .u8(u8::from(negative))
            .u32(days)
            .u8(hours)
            .u8(minutes)
            .u8(seconds);
    }
}
