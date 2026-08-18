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
            Value::F32(v) => {
                w.lenenc_str(&format_f64(*v as f64));
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
            // Everything else — strings, decimals, dates, JSON, blobs — is
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
    if v.is_nan() {
        "NaN".to_owned()
    } else if v.is_infinite() {
        if v.is_sign_negative() {
            "-Inf".to_owned()
        } else {
            "Inf".to_owned()
        }
    } else {
        let mut s = v.to_string();
        if let Some(stripped) = s.strip_suffix(".0") {
            s = stripped.to_owned();
        }
        s
    }
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
