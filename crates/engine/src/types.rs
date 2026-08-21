//! The SQL type system, and coercion of values into declared column types.

use mysql_ultralight_protocol::ColumnType;
use sqlparser::ast::DataType;

use crate::error::{unsupported, EngineError, EngineResult};
use crate::value::Value;

/// A column's declared type.
///
/// Deliberately coarser than MySQL's full type list: types that behave
/// identically for storage and comparison collapse together, and the original
/// spelling is kept in `declared` so `information_schema` and result metadata
/// can report what the user actually wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlType {
    pub kind: TypeKind,
    /// The type as written in the DDL, e.g. `varchar(21)` or `datetime(3)`.
    pub declared: String,
    /// Display length or precision, where the type has one.
    pub length: Option<u64>,
    pub scale: Option<u64>,
    pub unsigned: bool,
    /// Permitted values for ENUM and SET.
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Bool,
    Int,
    Float,
    Decimal,
    Text,
    Blob,
    Date,
    DateTime,
    Time,
    Year,
    Json,
    Enum,
    Set,
}

impl SqlType {
    fn simple(kind: TypeKind, declared: &str) -> Self {
        Self {
            kind,
            declared: declared.to_owned(),
            length: None,
            scale: None,
            unsigned: false,
            variants: Vec::new(),
        }
    }

    /// Maps a parsed type to the engine's representation.
    pub fn from_ast(ty: &DataType) -> EngineResult<Self> {
        use DataType as D;

        let mut out = match ty {
            D::Boolean | D::Bool => Self::simple(TypeKind::Bool, "tinyint(1)"),
            D::TinyInt(n) => int_type("tinyint", *n, false),
            D::TinyIntUnsigned(n) => int_type("tinyint", *n, true),
            D::SmallInt(n) | D::Int2(n) => int_type("smallint", *n, false),
            D::SmallIntUnsigned(n) | D::Int2Unsigned(n) => int_type("smallint", *n, true),
            D::MediumInt(n) => int_type("mediumint", *n, false),
            D::MediumIntUnsigned(n) => int_type("mediumint", *n, true),
            D::Int(n) | D::Integer(n) | D::Int4(n) => int_type("int", *n, false),
            D::IntUnsigned(n) | D::IntegerUnsigned(n) | D::Int4Unsigned(n) => {
                int_type("int", *n, true)
            }
            D::BigInt(n) | D::Int8(n) => int_type("bigint", *n, false),
            D::BigIntUnsigned(n) | D::Int8Unsigned(n) => int_type("bigint", *n, true),
            // `REAL` is a synonym for DOUBLE in MySQL, not for FLOAT — only
            // `FLOAT4` is the narrow one.
            D::Float4 => Self::simple(TypeKind::Float, "float"),
            D::Real => Self::simple(TypeKind::Float, "double"),
            D::RealUnsigned | D::DoublePrecisionUnsigned => {
                float_type("double", &sqlparser::ast::ExactNumberInfo::None, true)
            }
            D::Float(info) => float_type("float", info, false),
            D::FloatUnsigned(info) => float_type("float", info, true),
            D::DoublePrecision | D::Float8 => Self::simple(TypeKind::Float, "double"),
            D::Double(info) => float_type("double", info, false),
            D::DoubleUnsigned(info) => float_type("double", info, true),
            D::Decimal(info) | D::Numeric(info) | D::Dec(info) => decimal_type(info, false),
            D::DecimalUnsigned(info) | D::DecUnsigned(info) => decimal_type(info, true),
            D::Char(n) | D::Character(n) => char_type("char", n.as_ref().map(character_len)),
            D::Varchar(n) | D::CharacterVarying(n) | D::CharVarying(n) => {
                char_type("varchar", n.as_ref().map(character_len))
            }
            D::Text => Self::simple(TypeKind::Text, "text"),
            D::TinyText => Self::simple(TypeKind::Text, "tinytext"),
            D::MediumText => Self::simple(TypeKind::Text, "mediumtext"),
            D::LongText => Self::simple(TypeKind::Text, "longtext"),
            D::Blob(_) => Self::simple(TypeKind::Blob, "blob"),
            D::TinyBlob => Self::simple(TypeKind::Blob, "tinyblob"),
            D::MediumBlob => Self::simple(TypeKind::Blob, "mediumblob"),
            D::LongBlob | D::Bytea => Self::simple(TypeKind::Blob, "longblob"),
            // A bare `BINARY` is `BINARY(1)` to MySQL, which reports the 1.
            D::Binary(n) => {
                let length = n.unwrap_or(1);
                let mut t = Self::simple(TypeKind::Blob, "binary");
                t.declared = format!("binary({length})");
                t.length = Some(length);
                t
            }
            // The length is the whole difference between `VARBINARY(9)` and
            // a 64KB blob, and `CHARACTER_MAXIMUM_LENGTH` is read from it.
            D::Varbinary(n) => {
                let mut t = Self::simple(TypeKind::Blob, "varbinary");
                if let Some(sqlparser::ast::BinaryLength::IntegerLength { length }) = n {
                    t.declared = format!("varbinary({length})");
                    t.length = Some(*length);
                }
                t
            }
            D::Date => Self::simple(TypeKind::Date, "date"),
            D::Datetime(n) => datetime_type("datetime", *n),
            D::Timestamp(n, _) => datetime_type("timestamp", *n),
            D::Time(n, _) => datetime_type("time", *n),
            D::JSON => Self::simple(TypeKind::Json, "json"),
            D::Enum(values, _) => {
                let variants: Vec<String> = values
                    .iter()
                    .map(|v| match v {
                        sqlparser::ast::EnumMember::Name(n) => n.clone(),
                        sqlparser::ast::EnumMember::NamedValue(n, _) => n.clone(),
                    })
                    .collect();
                let mut t = Self::simple(TypeKind::Enum, "enum");
                t.declared = format!("enum({})", quoted_members(&variants));
                t.variants = variants;
                t
            }
            D::Set(values) => {
                let mut t = Self::simple(TypeKind::Set, "set");
                // Not trimmed: `sqlparser` has already unquoted these, so
                // a trim only reaches a member whose own text begins or
                // ends with a quote, and strips the member's own quotes.
                let variants: Vec<String> = values.clone();
                // `sqlparser` hands the members over unquoted, and MySQL
                // reports them quoted: `set(x,y)` is not what any client that
                // parses `COLUMN_TYPE` expects to read.
                t.declared = format!("set({})", quoted_members(&variants));
                t.variants = variants;
                t
            }
            D::Custom(name, _) => {
                // YEAR reaches us as a custom type rather than a known one.
                if name.to_string().eq_ignore_ascii_case("year") {
                    Self::simple(TypeKind::Year, "year")
                } else {
                    return Err(unsupported(format!("column type `{name}`")));
                }
            }
            other => return Err(unsupported(format!("column type `{other}`"))),
        };

        if out.declared.is_empty() {
            out.declared = format!("{ty}").to_lowercase();
        }
        Ok(out)
    }

    /// Recovers a type from a wire type code.
    ///
    /// Used where a result set is fed back in as a relation, such as a view or
    /// a derived table: without it every column would be reported as text and
    /// a typed client would decode a number as a string.
    pub fn from_column_type(ty: ColumnType) -> Self {
        let simple = |kind, declared: &str| SqlType {
            kind,
            declared: declared.to_owned(),
            length: None,
            scale: None,
            unsigned: false,
            variants: Vec::new(),
        };
        match ty {
            ColumnType::Tiny => simple(TypeKind::Bool, "tinyint(1)"),
            ColumnType::Short => simple(TypeKind::Int, "smallint"),
            ColumnType::Long | ColumnType::Int24 => simple(TypeKind::Int, "int"),
            ColumnType::LongLong => simple(TypeKind::Int, "bigint"),
            ColumnType::Float => simple(TypeKind::Float, "float"),
            ColumnType::Double => simple(TypeKind::Float, "double"),
            ColumnType::Decimal | ColumnType::NewDecimal => simple(TypeKind::Decimal, "decimal"),
            ColumnType::Date => simple(TypeKind::Date, "date"),
            ColumnType::DateTime | ColumnType::Timestamp => simple(TypeKind::DateTime, "datetime"),
            ColumnType::Time => simple(TypeKind::Time, "time"),
            ColumnType::Year => simple(TypeKind::Year, "year"),
            ColumnType::Json => simple(TypeKind::Json, "json"),
            ColumnType::Blob
            | ColumnType::LongBlob
            | ColumnType::MediumBlob
            | ColumnType::TinyBlob => simple(TypeKind::Blob, "blob"),
            // The width is carried as well as spelled, so `COLUMN_TYPE` and
            // `CHARACTER_MAXIMUM_LENGTH` agree: with `length: None` the pair
            // said `varchar(512)` and 65535, two different widths for one
            // column.
            _ => SqlType {
                length: Some(512),
                ..simple(TypeKind::Text, "varchar(512)")
            },
        }
    }

    /// Whether values of this type are text rather than bytes.
    ///
    /// Not derivable from the type code: `TEXT` and `BLOB` are both
    /// `MYSQL_TYPE_BLOB` and only the charset separates them. JSON travels as
    /// bytes, which is what MySQL reports for it too.
    pub fn is_textual(&self) -> bool {
        matches!(self.kind, TypeKind::Text | TypeKind::Enum | TypeKind::Set)
    }

    /// The MySQL type code reported in result set metadata.
    ///
    /// By the declared type rather than the kind, because MySQL's codes are
    /// finer than this engine's kinds: an `INT` is `LONG` and a `BIGINT` is
    /// `LONGLONG`, a `CHAR` is `STRING` where a `VARCHAR` is `VAR_STRING`, and
    /// every `TEXT` is a `BLOB`. Reporting one code for a whole kind told a
    /// typed client the wrong width for nearly every column.
    ///
    /// This is what the binary protocol encodes by, so the code and what the
    /// column can hold have to agree — an `INT` may say `LONG` only because
    /// `coerce` refuses anything four bytes cannot hold.
    pub fn column_type(&self) -> ColumnType {
        match self.kind {
            TypeKind::Bool => ColumnType::Tiny,
            TypeKind::Int => match self.base_name().as_str() {
                "tinyint" => ColumnType::Tiny,
                "smallint" => ColumnType::Short,
                "mediumint" => ColumnType::Int24,
                "bigint" => ColumnType::LongLong,
                _ => ColumnType::Long,
            },
            // FLOAT stays a double: the engine holds one, and saying `FLOAT`
            // would have the binary protocol write four bytes for it. #0048.
            TypeKind::Float => ColumnType::Double,
            TypeKind::Decimal => ColumnType::NewDecimal,
            // A fixed-width string is `STRING`, a varying one `VAR_STRING`,
            // and every size of TEXT is a `BLOB` — MySQL does not distinguish
            // the text blobs from the binary ones in the type code, only in
            // the charset.
            TypeKind::Text => match self.base_name().as_str() {
                "char" => ColumnType::String,
                "tinytext" | "text" | "mediumtext" | "longtext" => ColumnType::Blob,
                _ => ColumnType::VarString,
            },
            TypeKind::Enum | TypeKind::Set => ColumnType::String,
            TypeKind::Blob => match self.base_name().as_str() {
                "binary" => ColumnType::String,
                "varbinary" => ColumnType::VarString,
                _ => ColumnType::Blob,
            },
            TypeKind::Date => ColumnType::Date,
            TypeKind::DateTime => match self.base_name().as_str() {
                "timestamp" => ColumnType::Timestamp,
                _ => ColumnType::DateTime,
            },
            TypeKind::Time => ColumnType::Time,
            TypeKind::Year => ColumnType::Year,
            TypeKind::Json => ColumnType::Json,
        }
    }

    /// The `data_type` column of `information_schema.columns`: the bare name,
    /// without length, precision or signedness.
    ///
    /// `declared` holds what MySQL reports as `COLUMN_TYPE`, which for an
    /// integer carries a trailing ` unsigned` — so the cut is at the first
    /// space as well as the first parenthesis. Everything that dispatches on
    /// the spelling comes through here, so missing the space would make
    /// `bigint unsigned` an unrecognised type rather than a `bigint`.
    pub fn base_name(&self) -> String {
        let end = self
            .declared
            .find(['(', ' '])
            .unwrap_or(self.declared.len());
        self.declared[..end].to_owned()
    }

    /// Converts a value to this column's type, or explains why it cannot.
    ///
    /// Rejecting rather than silently storing the wrong shape is the point:
    /// a test suite cannot notice a value that was quietly coerced.
    /// The value MySQL stores when a NOT NULL column with no default is left
    /// out of an `INSERT IGNORE`.
    ///
    /// Under a strict `sql_mode` the plain statement is an error, but IGNORE
    /// downgrades it to a warning and writes this instead — which is how the
    /// real migrations get away with omitting `NOT NULL` columns.
    pub fn implicit_default(&self) -> Value {
        match self.kind {
            TypeKind::Bool | TypeKind::Int | TypeKind::Year => Value::Int(0),
            TypeKind::Float => Value::Float(0.0),
            // At the column's scale, and tagged, because this value goes into
            // a row without passing `coerce`. Left as plain text it was a
            // second spelling of zero — a group of its own beside `0.00`, and
            // one inexact value in the column, which sent `SUM` over *every*
            // row back through the float path.
            TypeKind::Decimal => {
                Value::Decimal(crate::decimal::render(0, self.scale.unwrap_or(0) as usize))
            }
            // The epoch rather than MySQL's `0000-00-00`, because the mode this
            // engine targets sets NO_ZERO_DATE and would reject that value.
            TypeKind::Date => Value::text("1970-01-01"),
            TypeKind::DateTime => Value::text("1970-01-01 00:00:00"),
            TypeKind::Time => Value::text("00:00:00"),
            // Tagged, so a column filled from its default holds a document
            // like every other row does.
            TypeKind::Json => Value::Json("null".to_owned()),
            // An ENUM's implicit default is its first member, not an empty
            // string, which would not be a legal value at all.
            TypeKind::Enum => match self.variants.first() {
                Some(first) => Value::text(first),
                None => Value::text(""),
            },
            TypeKind::Text | TypeKind::Blob | TypeKind::Set => Value::text(""),
        }
    }

    /// The values this integer column can hold.
    ///
    /// MySQL sizes an integer by its declared width and refuses anything
    /// outside it, which is not a formality here: the binary protocol encodes
    /// by the declared type, so a `TINYINT` that had been allowed to hold 999
    /// would reach a client as 231.
    fn integer_range(&self) -> (i64, i64) {
        let (signed_low, signed_high, unsigned_high) = match self.base_name().as_str() {
            "tinyint" => (-128, 127, 255),
            "smallint" => (-32_768, 32_767, 65_535),
            "mediumint" => (-8_388_608, 8_388_607, 16_777_215),
            "int" | "integer" => (-2_147_483_648, 2_147_483_647, 4_294_967_295),
            // The engine holds an `i64`, so an UNSIGNED BIGINT above
            // `i64::MAX` is out of reach whatever this says.
            _ => (i64::MIN, i64::MAX, i64::MAX),
        };
        if self.unsigned {
            (0, unsigned_high)
        } else {
            (signed_low, signed_high)
        }
    }

    pub fn coerce(&self, value: Value, column: &str) -> EngineResult<Value> {
        if value.is_null() {
            return Ok(Value::Null);
        }

        let mismatch = || {
            EngineError::new(
                1366,
                "HY000",
                format!(
                    "Incorrect {} value for column '{}'",
                    self.base_name(),
                    column
                ),
            )
        };

        let out_of_range = || {
            EngineError::new(
                1264,
                "22003",
                format!("Out of range value for column '{column}' at row 1"),
            )
        };

        Ok(match self.kind {
            TypeKind::Bool | TypeKind::Int => {
                let n = value.as_int().ok_or_else(mismatch)?;
                let (low, high) = self.integer_range();
                if n < low || n > high {
                    return Err(out_of_range());
                }
                Value::Int(n)
            }
            // A YEAR is two bytes on the wire, and MySQL's range for it is
            // 1901 to 2155 with zero for "none". Left unchecked it was the one
            // integer that could still outgrow the code describing it: 99999
            // reaches a binary client as 34463.
            TypeKind::Year => {
                let n = value.as_int().ok_or_else(mismatch)?;
                if n != 0 && !(1901..=2155).contains(&n) {
                    return Err(out_of_range());
                }
                Value::Int(n)
            }
            TypeKind::Float => {
                let n = value.as_float().ok_or_else(mismatch)?;
                if self.unsigned && n < 0.0 {
                    return Err(out_of_range());
                }
                Value::Float(n)
            }
            // Decimals keep their text form so no precision is lost on the way
            // through, which is the whole reason MySQL transmits them as text.
            TypeKind::Decimal => {
                let text = value.as_text();
                // By the text rather than a parsed float: the point of keeping
                // the text is that parsing loses precision, and the sign is
                // readable without it.
                let stored = match text.strip_prefix('-') {
                    // `-0.00` is not below zero, so MySQL takes it — and
                    // stores it without the sign, which is why the digits
                    // decide rather than the sign character.
                    Some(digits) if digits.chars().all(|c| c == '0' || c == '.') => {
                        digits.to_owned()
                    }
                    Some(_) if self.unsigned => return Err(out_of_range()),
                    _ => text,
                };
                // At the column's own scale, which is what a DECIMAL *is*:
                // 1.5 in a `DECIMAL(6,2)` is `1.50` and 1.005 is `1.01`.
                // Keeping whatever the caller wrote made the scale a property
                // of the statement rather than of the column.
                let scale = self.scale.unwrap_or(0) as usize;
                let digits = crate::decimal::rescale(&stored, scale).unwrap_or_else(|| {
                    // Not a decimal at all, so the number MySQL reads out of
                    // it — which is 0 for a name and clamped for an overflow.
                    crate::decimal::from_f64(Value::text(&stored).as_number(), scale)
                });
                Value::Decimal(digits)
            }
            TypeKind::Blob => match value {
                Value::Bytes(b) => Value::Bytes(b),
                other => Value::Bytes(other.as_text().into_bytes()),
            },
            TypeKind::Enum => {
                let text = value.as_text();
                if !self.variants.is_empty() && !self.variants.contains(&text) {
                    return Err(EngineError::new(
                        1265,
                        "01000",
                        format!("Data truncated for column '{column}'"),
                    ));
                }
                Value::Text(text)
            }
            // A JSON column holds a document, so reading one and passing it to
            // a JSON function embeds it as a document. MySQL validates on the
            // way in, and so does this: a column that accepted `{"a":1` would
            // fail every later read of it instead, far from the cause.
            TypeKind::Json => {
                let text = value.as_text();
                // Serde's own message, because "invalid" without a position is
                // no help against a document of any size — and the sibling
                // 3141 raised by `json::parse` already says where.
                let parsed = serde_json::from_str::<serde_json::Value>(&text).map_err(|e| {
                    EngineError::new(
                        3140,
                        "22032",
                        format!("Invalid JSON text: \"{e}\" in column '{column}'"),
                    )
                })?;
                // Stored as the document, not as the text that was written.
                // MySQL holds JSON in a parsed form, so `{"b":1,  "a":2}` reads
                // back as `{"a": 2, "b": 1}` — whitespace gone and keys in its
                // own order. Keeping the input verbatim made every column a
                // record of how it happened to be typed.
                Value::Json(crate::json::render_text(&parsed))
            }
            _ => Value::Text(value.as_text()),
        })
    }
}

fn int_type(name: &str, display: Option<u64>, unsigned: bool) -> SqlType {
    // MySQL 8 deprecated the display width and stopped reporting it: `INT(11)`
    // reads back as `int`. The one survivor is a signed `TINYINT(1)`, because
    // that spelling is how a boolean is written and every client that maps one
    // reads `COLUMN_TYPE` to find it — `TINYINT(1) UNSIGNED` is not that, and
    // MySQL reports it as plain `tinyint unsigned`.
    let mut declared = if name == "tinyint" && display == Some(1) && !unsigned {
        "tinyint(1)".to_owned()
    } else {
        name.to_owned()
    };
    if unsigned {
        declared.push_str(" unsigned");
    }
    SqlType {
        kind: TypeKind::Int,
        declared,
        length: display,
        scale: None,
        unsigned,
        variants: Vec::new(),
    }
}

/// The `(M)` or `(M,D)` a numeric type was declared with.
fn exact_number(info: &sqlparser::ast::ExactNumberInfo) -> (Option<u64>, Option<u64>) {
    use sqlparser::ast::ExactNumberInfo as E;
    match info {
        E::None => (None, None),
        E::Precision(p) => (Some(*p), None),
        // sqlparser models scale as signed; MySQL scales are non-negative.
        E::PrecisionAndScale(p, s) => (Some(*p), Some((*s).max(0) as u64)),
    }
}

/// `COLUMN_TYPE` for a numeric: the name, what the DDL wrote, and the sign.
fn numeric_declared(
    name: &str,
    precision: Option<u64>,
    scale: Option<u64>,
    unsigned: bool,
) -> String {
    let mut declared = match (precision, scale) {
        (Some(p), Some(s)) => format!("{name}({p},{s})"),
        (Some(p), None) => format!("{name}({p})"),
        _ => name.to_owned(),
    };
    if unsigned {
        declared.push_str(" unsigned");
    }
    declared
}

fn decimal_type(info: &sqlparser::ast::ExactNumberInfo, unsigned: bool) -> SqlType {
    let (precision, scale) = exact_number(info);
    // MySQL fills both in and reports them: a bare `DECIMAL` is `decimal(10,0)`
    // and `DECIMAL(8)` is `decimal(8,0)`. Filling them here rather than at the
    // point of display keeps `COLUMN_TYPE` and the precision and scale columns
    // reading off the same numbers.
    let precision = Some(precision.unwrap_or(10));
    let scale = Some(scale.unwrap_or(0));
    SqlType {
        kind: TypeKind::Decimal,
        declared: numeric_declared("decimal", precision, scale, unsigned),
        length: precision,
        scale,
        unsigned,
        variants: Vec::new(),
    }
}

/// A `FLOAT` or `DOUBLE`.
///
/// Only the two-argument `(M,D)` form survives into `COLUMN_TYPE`, and it is
/// the only one with a fixed number of decimal places — MySQL reports
/// `NUMERIC_SCALE` as NULL for the rest, so the two cases have to stay
/// distinguishable.
///
/// A precision on its own means something else entirely: `FLOAT(p)` asks for
/// at least `p` bits of mantissa, so MySQL answers with whichever of the two
/// storage sizes provides it and reports *that* type, with no `(p)`. The
/// boundary is measured rather than taken from the manual, which says 24 where
/// 8.3 promotes at 25.
fn float_type(name: &str, info: &sqlparser::ast::ExactNumberInfo, unsigned: bool) -> SqlType {
    let (precision, scale) = exact_number(info);
    let (name, precision, scale) = match (precision, scale) {
        (Some(_), Some(_)) => (name, precision, scale),
        // Only `FLOAT` is asked for in bits. A single-argument `DOUBLE` is not
        // legal MySQL at all, so there is no width to promote to and no name
        // to change — answering `float` there would report a narrower type
        // than the DDL asked for, which is the failure this file exists to
        // stop.
        (Some(bits), None) if name == "float" => {
            (if bits >= 25 { "double" } else { "float" }, None, None)
        }
        _ => (name, None, None),
    };
    SqlType {
        kind: TypeKind::Float,
        declared: numeric_declared(name, precision, scale, unsigned),
        length: precision,
        scale,
        unsigned,
        variants: Vec::new(),
    }
}

/// ENUM and SET members as MySQL reports them: quoted, with any quote inside
/// one doubled, which is the same escaping the DDL that declared them used.
fn quoted_members(variants: &[String]) -> String {
    variants
        .iter()
        .map(|v| format!("'{}'", v.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",")
}

fn char_type(name: &str, length: Option<u64>) -> SqlType {
    let declared = match length {
        Some(n) => format!("{name}({n})"),
        None => name.to_owned(),
    };
    SqlType {
        kind: TypeKind::Text,
        declared,
        length,
        scale: None,
        unsigned: false,
        variants: Vec::new(),
    }
}

fn datetime_type(name: &str, precision: Option<u64>) -> SqlType {
    let declared = match precision {
        Some(n) => format!("{name}({n})"),
        None => name.to_owned(),
    };
    let kind = match name {
        "date" => TypeKind::Date,
        "time" => TypeKind::Time,
        _ => TypeKind::DateTime,
    };
    SqlType {
        kind,
        declared,
        length: precision,
        scale: None,
        unsigned: false,
        variants: Vec::new(),
    }
}

fn character_len(length: &sqlparser::ast::CharacterLength) -> u64 {
    use sqlparser::ast::CharacterLength as C;
    match length {
        C::IntegerLength { length, .. } => *length,
        C::Max => 0,
    }
}
