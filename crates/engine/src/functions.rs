//! Scalar functions, LIKE matching, and CAST.
//!
//! Aggregates are not here: they need the whole group rather than one row, so
//! the executor handles them.

use sqlparser::ast::DataType;

use crate::error::{unsupported, EngineResult};
use crate::session::Session;
use crate::value::Value;

/// Dispatches a scalar function call.
pub fn call(name: &str, args: Vec<Value>, session: &Session) -> EngineResult<Value> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
    let text = |i: usize| arg(i).as_text();

    Ok(match name {
        // -- null handling ------------------------------------------------
        "coalesce" => args
            .into_iter()
            .find(|v| !v.is_null())
            .unwrap_or(Value::Null),
        "ifnull" | "nvl" => {
            if arg(0).is_null() {
                arg(1)
            } else {
                arg(0)
            }
        }
        "nullif" => {
            if arg(0).compare(&arg(1)) == Some(std::cmp::Ordering::Equal) {
                Value::Null
            } else {
                arg(0)
            }
        }
        "if" => match arg(0).truthy() {
            Some(true) => arg(1),
            _ => arg(2),
        },
        "isnull" => Value::Int(i64::from(arg(0).is_null())),

        // -- strings -------------------------------------------------------
        "concat" => {
            // NULL poisons the whole concatenation, unlike CONCAT_WS.
            if args.iter().any(Value::is_null) {
                Value::Null
            } else {
                Value::Text(args.iter().map(Value::as_text).collect())
            }
        }
        "concat_ws" => {
            let sep = text(0);
            let joined: Vec<String> = args
                .iter()
                .skip(1)
                .filter(|v| !v.is_null())
                .map(Value::as_text)
                .collect();
            Value::Text(joined.join(&sep))
        }
        "length" | "octet_length" => Value::Int(text(0).len() as i64),
        "char_length" | "character_length" => Value::Int(text(0).chars().count() as i64),
        "upper" | "ucase" => Value::Text(text(0).to_uppercase()),
        "lower" | "lcase" => Value::Text(text(0).to_lowercase()),
        "trim" => Value::Text(text(0).trim().to_owned()),
        "ltrim" => Value::Text(text(0).trim_start().to_owned()),
        "rtrim" => Value::Text(text(0).trim_end().to_owned()),
        "reverse" => Value::Text(text(0).chars().rev().collect()),
        "space" => Value::Text(" ".repeat(arg(0).as_int().unwrap_or(0).max(0) as usize)),
        "replace" => Value::Text(text(0).replace(&text(1), &text(2))),
        "left" => {
            let n = arg(1).as_int().unwrap_or(0).max(0) as usize;
            Value::Text(text(0).chars().take(n).collect())
        }
        "right" => {
            let s = text(0);
            let n = arg(1).as_int().unwrap_or(0).max(0) as usize;
            let skip = s.chars().count().saturating_sub(n);
            Value::Text(s.chars().skip(skip).collect())
        }
        "substring" | "substr" | "mid" => {
            let s: Vec<char> = text(0).chars().collect();
            // MySQL positions are 1-based, and a negative start counts back
            // from the end.
            let start = arg(1).as_int().unwrap_or(1);
            let from = if start < 0 {
                s.len().saturating_sub(start.unsigned_abs() as usize)
            } else {
                (start.max(1) as usize) - 1
            };
            let take = match args.get(2) {
                Some(v) => v.as_int().unwrap_or(0).max(0) as usize,
                None => s.len(),
            };
            Value::Text(s.into_iter().skip(from).take(take).collect())
        }
        "instr" | "locate" | "position" => {
            let (haystack, needle) = if name == "instr" {
                (text(0), text(1))
            } else {
                (text(1), text(0))
            };
            Value::Int(haystack.find(&needle).map_or(0, |i| i as i64 + 1))
        }
        "lpad" | "rpad" => {
            let s = text(0);
            let width = arg(1).as_int().unwrap_or(0).max(0) as usize;
            let pad = if args.len() > 2 {
                text(2)
            } else {
                " ".to_owned()
            };
            Value::Text(pad_to(&s, width, &pad, name == "lpad"))
        }

        // -- numbers -------------------------------------------------------
        "abs" => match arg(0) {
            Value::Int(v) => Value::Int(v.abs()),
            other => other
                .as_float()
                .map(|f| Value::Float(f.abs()))
                .unwrap_or(Value::Null),
        },
        "round" => {
            let places = args.get(1).and_then(Value::as_int).unwrap_or(0);
            match arg(0).as_float() {
                Some(f) => {
                    let factor = 10f64.powi(places as i32);
                    let rounded = (f * factor).round() / factor;
                    if places <= 0 {
                        Value::Int(rounded as i64)
                    } else {
                        Value::Float(rounded)
                    }
                }
                None => Value::Null,
            }
        }
        "floor" => arg(0)
            .as_float()
            .map(|f| Value::Int(f.floor() as i64))
            .unwrap_or(Value::Null),
        "ceil" | "ceiling" => arg(0)
            .as_float()
            .map(|f| Value::Int(f.ceil() as i64))
            .unwrap_or(Value::Null),
        "mod" => match (arg(0).as_int(), arg(1).as_int()) {
            (Some(_), Some(0)) | (None, _) | (_, None) => Value::Null,
            (Some(a), Some(b)) => Value::Int(a % b),
        },
        "greatest" | "least" => {
            if args.iter().any(Value::is_null) {
                Value::Null
            } else {
                let want_greater = name == "greatest";
                args.into_iter()
                    .reduce(|a, b| {
                        let take_b = match a.compare(&b) {
                            Some(std::cmp::Ordering::Less) => want_greater,
                            Some(std::cmp::Ordering::Greater) => !want_greater,
                            _ => false,
                        };
                        if take_b {
                            b
                        } else {
                            a
                        }
                    })
                    .unwrap_or(Value::Null)
            }
        }

        // -- session and server --------------------------------------------
        "database" | "schema" => match &session.current_database {
            Some(db) => Value::Text(db.clone()),
            None => Value::Null,
        },
        "version" => Value::Text(session.config.version.clone()),
        "connection_id" => Value::Int(session.connection_id as i64),
        "last_insert_id" => Value::Int(session.last_insert_id as i64),
        "user" | "current_user" | "session_user" | "system_user" => {
            Value::Text("root@localhost".to_owned())
        }
        "row_count" => Value::Int(session.affected_rows as i64),

        // -- time ------------------------------------------------------------
        "now" | "current_timestamp" | "localtime" | "localtimestamp" | "sysdate" => {
            Value::Text(now_datetime())
        }
        "curdate" | "current_date" => Value::Text(now_datetime()[..10].to_owned()),
        "curtime" | "current_time" => Value::Text(now_datetime()[11..].to_owned()),
        "unix_timestamp" => Value::Int(unix_seconds() as i64),
        "date" => {
            let s = text(0);
            Value::Text(s.chars().take(10).collect())
        }
        "year" => parse_date_part(&text(0), 0),
        "month" => parse_date_part(&text(0), 1),
        "day" | "dayofmonth" => parse_date_part(&text(0), 2),

        other => return Err(unsupported(format!("function `{other}()`"))),
    })
}

fn pad_to(s: &str, width: usize, pad: &str, left: bool) -> String {
    let len = s.chars().count();
    if len >= width || pad.is_empty() {
        return s.chars().take(width).collect();
    }
    let filler: String = pad.chars().cycle().take(width - len).collect();
    if left {
        format!("{filler}{s}")
    } else {
        format!("{s}{filler}")
    }
}

fn parse_date_part(s: &str, index: usize) -> Value {
    match s
        .split(['-', ' ', ':'])
        .nth(index)
        .and_then(|p| p.parse::<i64>().ok())
    {
        Some(v) => Value::Int(v),
        None => Value::Null,
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The current UTC time as `YYYY-MM-DD HH:MM:SS`.
///
/// Hand-rolled rather than pulling in a date crate: the engine needs one
/// format, and a dependency here would be paid on every cold start.
pub fn now_datetime() -> String {
    format_unix(unix_seconds())
}

/// Converts a Unix timestamp to `YYYY-MM-DD HH:MM:SS` using the civil-from-days
/// algorithm, which is correct across leap years and centuries.
pub fn format_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{year:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Applies a CAST.
pub fn cast(value: Value, ty: &DataType) -> EngineResult<Value> {
    use DataType as D;
    if value.is_null() {
        return Ok(Value::Null);
    }
    Ok(match ty {
        D::TinyInt(_)
        | D::SmallInt(_)
        | D::Int(_)
        | D::Integer(_)
        | D::BigInt(_)
        | D::MediumInt(_) => value.as_int().map(Value::Int).unwrap_or(Value::Null),
        D::Float(_) | D::Double(_) | D::DoublePrecision | D::Real => {
            value.as_float().map(Value::Float).unwrap_or(Value::Null)
        }
        D::Decimal(_) | D::Numeric(_) | D::Dec(_) => Value::Text(value.as_text()),
        D::Char(_) | D::Varchar(_) | D::Text | D::CharacterVarying(_) | D::Character(_) => {
            Value::Text(value.as_text())
        }
        D::Date => Value::Text(value.as_text().chars().take(10).collect()),
        D::Datetime(_) | D::Timestamp(_, _) | D::Time(_, _) | D::JSON => {
            Value::Text(value.as_text())
        }
        D::Binary(_) | D::Varbinary(_) | D::Blob(_) => Value::Bytes(value.as_text().into_bytes()),
        other => return Err(unsupported(format!("CAST to `{other}`"))),
    })
}

/// MySQL LIKE matching: `%` is any run, `_` is one character.
///
/// Backtracking rather than regex, so there is no dependency and no pattern
/// compilation cost on a per-row call.
pub fn like(text: &str, pattern: &str, escape: Option<char>) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    like_at(&t, 0, &p, 0, escape)
}

fn like_at(t: &[char], mut ti: usize, p: &[char], mut pi: usize, escape: Option<char>) -> bool {
    while pi < p.len() {
        let ch = p[pi];

        if Some(ch) == escape {
            // An escaped character must match literally.
            let Some(next) = p.get(pi + 1) else {
                return false;
            };
            if ti >= t.len() || t[ti] != *next {
                return false;
            }
            ti += 1;
            pi += 2;
            continue;
        }

        match ch {
            '%' => {
                // Try every split point; the shortest match wins first.
                for skip in ti..=t.len() {
                    if like_at(t, skip, p, pi + 1, escape) {
                        return true;
                    }
                }
                return false;
            }
            '_' => {
                if ti >= t.len() {
                    return false;
                }
                ti += 1;
                pi += 1;
            }
            literal => {
                if ti >= t.len() || t[ti] != literal {
                    return false;
                }
                ti += 1;
                pi += 1;
            }
        }
    }
    ti == t.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_handles_wildcards_and_anchoring() {
        assert!(like("hello", "hello", None));
        assert!(like("hello", "h%", None));
        assert!(like("hello", "%o", None));
        assert!(like("hello", "h_llo", None));
        assert!(like("hello", "%ell%", None));
        assert!(!like("hello", "h_lo", None));
        // A pattern with no wildcard must match the whole string.
        assert!(!like("hello world", "hello", None));
        assert!(like("", "%", None));
    }

    #[test]
    fn like_escape_makes_a_wildcard_literal() {
        assert!(like("100%", "100\\%", Some('\\')));
        assert!(!like("1000", "100\\%", Some('\\')));
        assert!(like("a_b", "a\\_b", Some('\\')));
        assert!(!like("axb", "a\\_b", Some('\\')));
    }

    /// Backtracking must not give up after the first `%` placement fails.
    #[test]
    fn like_backtracks_across_multiple_wildcards() {
        assert!(like("aaa", "%a%a%", None));
        assert!(!like("a", "%a%a%", None));
    }

    #[test]
    fn dates_convert_across_leap_years() {
        assert_eq!(format_unix(0), "1970-01-01 00:00:00");
        assert_eq!(format_unix(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(format_unix(1_709_164_800), "2024-02-29 00:00:00");
        assert_eq!(format_unix(1_735_689_599), "2024-12-31 23:59:59");
    }

    #[test]
    fn concat_is_null_poisoned_but_concat_ws_is_not() {
        let s = Session::new(crate::session::ServerConfig::default(), 1);
        let nulled = call("concat", vec![Value::text("a"), Value::Null], &s).unwrap();
        assert_eq!(nulled, Value::Null);

        let skipped = call(
            "concat_ws",
            vec![
                Value::text("-"),
                Value::text("a"),
                Value::Null,
                Value::text("b"),
            ],
            &s,
        )
        .unwrap();
        assert_eq!(skipped, Value::text("a-b"));
    }

    #[test]
    fn substring_is_one_based_and_accepts_a_negative_start() {
        let s = Session::new(crate::session::ServerConfig::default(), 1);
        let f = |args: Vec<Value>| call("substring", args, &s).unwrap();
        assert_eq!(
            f(vec![Value::text("abcdef"), Value::Int(2)]),
            Value::text("bcdef")
        );
        assert_eq!(
            f(vec![Value::text("abcdef"), Value::Int(2), Value::Int(3)]),
            Value::text("bcd")
        );
        assert_eq!(
            f(vec![Value::text("abcdef"), Value::Int(-2)]),
            Value::text("ef")
        );
    }

    #[test]
    fn an_unknown_function_is_an_error_rather_than_a_guess() {
        let s = Session::new(crate::session::ServerConfig::default(), 1);
        assert!(call("no_such_function", vec![], &s).is_err());
    }
}
