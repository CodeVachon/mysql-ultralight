//! Binding prepared-statement parameters into SQL text.
//!
//! Placeholders are substituted before parsing rather than during evaluation.
//! Evaluation order is not textual order — a `WHERE` clause may be evaluated
//! before a `LIMIT` that appears after it — so binding by evaluation order
//! would silently pair parameters with the wrong placeholders. Substituting
//! first makes position unambiguous.
//!
//! Where each `?` stood is found here and the substitution is applied in
//! `rewrite::bind`, which records the edit so a column can still be named
//! after the marker rather than after the value spliced into its place. See
//! that function for why the two halves are split.

use crate::value::Value;

/// The byte offset of every `?` that is a placeholder rather than data.
///
/// A question mark inside a string, an identifier or a comment is text the
/// client wrote, not a marker. `--`, `#` and `/* */` are all skipped: handling
/// one comment syntax and not another makes this disagree with whatever else
/// counts placeholders, and every later parameter then binds one position off.
pub fn placeholders(sql: &str) -> Vec<usize> {
    let bytes = sql.as_bytes();
    let mut at = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && q != b'`' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == q {
                        if bytes.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
            }
            b'#' | b'-' if bytes[i] == b'#' || bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'?' => {
                at.push(i);
                i += 1;
            }
            _ => i += utf8_len(bytes[i]),
        }
    }

    at
}

/// Renders a value as a SQL literal.
pub fn literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Int(v) => v.to_string(),
        // Unquoted and unsigned, so it re-parses as the number it is. MySQL
        // types a literal above `i64::MAX` BIGINT UNSIGNED, so the tag
        // survives this round trip where the temporal and ENUM ones below do
        // not.
        Value::Unsigned(v) => v.to_string(),
        Value::Float32(v) => literal(&Value::Float(*v as f64)),
        // Unquoted, so it re-parses as the number it is rather than as the
        // characters that spell it — and the digits it *carries*, not the ones
        // it shows: a parameter is substituted into SQL that is evaluated
        // again, and rendering it first would round the operand before the
        // statement saw it.
        Value::Decimal(v, _) => v.clone(),
        // Quoted, because SQL has no temporal literal without a keyword in
        // front of it — so this round trip loses the tag, exactly as the JSON
        // one below does. A bound parameter has no declared type to lose.
        Value::Temporal(v) => literal(&Value::text(v.clone())),
        // Quoted as its member, which loses the ordinal — the same round trip
        // the temporal above makes, and for the same reason: SQL has no
        // literal that spells an ENUM.
        Value::Enum { member, .. } => literal(&Value::text(member.clone())),
        // The keyword, which is the one tag here that SQL *can* spell — so
        // this round trip keeps it where the two above lose theirs. Writing
        // `1` would turn a bound boolean into a number, and against a JSON
        // column that is a different question with a different answer.
        Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
        Value::Float(v) => {
            // A bare `inf` or `nan` would not re-parse.
            if v.is_finite() {
                let s = v.to_string();
                if s.contains('.') || s.contains('e') {
                    s
                } else {
                    format!("{s}.0")
                }
            } else {
                "NULL".to_owned()
            }
        }
        Value::Text(v, _) => quote(v),
        // Quoted like text, because that is the only literal syntax MySQL has
        // for a document. A JSON column coerces it back on the way in — and
        // through `as_text`, so what is quoted is the document as MySQL writes
        // it rather than the stored form's marked spelling.
        Value::Json(_) => quote(&value.as_text()),
        // Hex literals avoid any question of encoding.
        Value::Bytes(v) => {
            let mut s = String::with_capacity(v.len() * 2 + 3);
            s.push_str("x'");
            for b in v {
                s.push_str(&format!("{b:02x}"));
            }
            s.push('\'');
            s
        }
    }
}

/// Single-quotes a string, escaping what MySQL requires.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\'' => out.push_str("''"),
            '\\' => out.push_str("\\\\"),
            '\0' => out.push_str("\\0"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bound statement, as the parser will see it.
    ///
    /// Through `rewrite::bind` because that is the only thing that binds: this
    /// module finds the placeholders and renders the values, and the pass that
    /// puts the two together lives there so the edit is recorded. Asserting on
    /// its text keeps these cases where the scanner they exercise is.
    fn bind(sql: &str, params: &[Value]) -> String {
        crate::rewrite::bind(sql, params).sql
    }

    #[test]
    fn binds_in_textual_order() {
        let sql = "SELECT * FROM t WHERE a = ? AND b = ? LIMIT ?";
        let bound = bind(sql, &[Value::Int(1), Value::text("x"), Value::Int(10)]);
        assert_eq!(bound, "SELECT * FROM t WHERE a = 1 AND b = 'x' LIMIT 10");
    }

    /// A question mark inside a literal or an identifier is data, not a
    /// placeholder; substituting it would corrupt the statement.
    #[test]
    fn question_marks_in_strings_and_identifiers_are_left_alone() {
        let bound = bind("SELECT '?', `we?rd`, ? FROM t", &[Value::Int(5)]);
        assert_eq!(bound, "SELECT '?', `we?rd`, 5 FROM t");
    }

    #[test]
    fn question_marks_in_comments_are_left_alone() {
        let bound = bind("SELECT ? -- ? here\n, 2", &[Value::Int(1)]);
        assert_eq!(bound, "SELECT 1 -- ? here\n, 2");
        let bound = bind("SELECT /* ? */ ?", &[Value::Int(1)]);
        assert_eq!(bound, "SELECT /* ? */ 1");
    }

    /// `#` is a line comment in MySQL. Skipping it here but not in the
    /// placeholder counter shifts every parameter after it.
    #[test]
    fn hash_comments_are_left_alone() {
        let bound = bind("SELECT ? # note ?\n, ?", &[Value::Int(1), Value::Int(2)]);
        assert_eq!(bound, "SELECT 1 # note ?\n, 2");
    }

    /// A boolean cannot reach here today — `Value::from_wire` builds
    /// integers and strings and never a `Value::Bool` — so this asserts the
    /// arm at its own level rather than through a statement that could not be
    /// written. It matters because `TRUE` is the one literal spelling that
    /// keeps the tag, and rendering `1` instead would turn a bound boolean
    /// into a number the moment a client protocol grows one.
    #[test]
    fn a_boolean_binds_as_the_keyword_that_keeps_its_tag() {
        assert_eq!(literal(&Value::Bool(true)), "TRUE");
        assert_eq!(literal(&Value::Bool(false)), "FALSE");
        assert_eq!(bind("SELECT ?", &[Value::Bool(true)]), "SELECT TRUE");
    }

    #[test]
    fn strings_are_escaped_so_they_cannot_break_out() {
        let bound = bind("SELECT ?", &[Value::text("it's \\ fine")]);
        assert_eq!(bound, r"SELECT 'it''s \\ fine'");
    }

    #[test]
    fn null_and_bytes_render_as_literals() {
        assert_eq!(bind("SELECT ?", &[Value::Null]), "SELECT NULL");
        assert_eq!(
            bind("SELECT ?", &[Value::Bytes(vec![0xde, 0xad])]),
            "SELECT x'dead'"
        );
    }

    #[test]
    fn a_missing_parameter_leaves_the_placeholder_for_the_parser_to_reject() {
        assert_eq!(bind("SELECT ?, ?", &[Value::Int(1)]), "SELECT 1, ?");
    }
}
