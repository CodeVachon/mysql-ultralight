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
            D::BigIntUnsigned(n) => int_type("bigint", *n, true),
            D::Float(_) | D::Real | D::Float4 => Self::simple(TypeKind::Float, "float"),
            D::Double(_) | D::DoublePrecision | D::Float8 => {
                Self::simple(TypeKind::Float, "double")
            }
            D::Decimal(info) | D::Numeric(info) | D::Dec(info) => decimal_type(info),
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
            D::Binary(n) => {
                let mut t = Self::simple(TypeKind::Blob, "binary");
                if let Some(n) = n {
                    t.declared = format!("binary({n})");
                    t.length = Some(*n);
                }
                t
            }
            D::Varbinary(_) => Self::simple(TypeKind::Blob, "varbinary"),
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
                t.declared = format!(
                    "enum({})",
                    variants
                        .iter()
                        .map(|v| format!("'{v}'"))
                        .collect::<Vec<_>>()
                        .join(",")
                );
                t.variants = variants;
                t
            }
            D::Set(values) => {
                let mut t = Self::simple(TypeKind::Set, "set");
                t.declared = format!("set({})", values.join(","));
                t.variants = values
                    .iter()
                    .map(|v| v.trim_matches('\'').to_owned())
                    .collect();
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
            _ => simple(TypeKind::Text, "varchar(512)"),
        }
    }

    /// The MySQL type code reported in result set metadata.
    pub fn column_type(&self) -> ColumnType {
        match self.kind {
            TypeKind::Bool => ColumnType::Tiny,
            TypeKind::Int => ColumnType::LongLong,
            TypeKind::Float => ColumnType::Double,
            TypeKind::Decimal => ColumnType::NewDecimal,
            TypeKind::Text | TypeKind::Enum | TypeKind::Set => ColumnType::VarString,
            TypeKind::Blob => ColumnType::Blob,
            TypeKind::Date => ColumnType::Date,
            TypeKind::DateTime => ColumnType::DateTime,
            TypeKind::Time => ColumnType::Time,
            TypeKind::Year => ColumnType::Year,
            TypeKind::Json => ColumnType::Json,
        }
    }

    /// The `data_type` column of `information_schema.columns`: the bare name,
    /// without length or precision.
    pub fn base_name(&self) -> String {
        match self.declared.split_once('(') {
            Some((base, _)) => base.trim().to_owned(),
            None => self.declared.clone(),
        }
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
            // Kept as text: a DECIMAL carries a scale that a float would lose.
            TypeKind::Decimal => Value::text("0"),
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

        Ok(match self.kind {
            TypeKind::Bool | TypeKind::Int | TypeKind::Year => {
                Value::Int(value.as_int().ok_or_else(mismatch)?)
            }
            TypeKind::Float => Value::Float(value.as_float().ok_or_else(mismatch)?),
            // Decimals keep their text form so no precision is lost on the way
            // through, which is the whole reason MySQL transmits them as text.
            TypeKind::Decimal => Value::Text(value.as_text()),
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
                if let Err(e) = serde_json::from_str::<serde_json::Value>(&text) {
                    return Err(EngineError::new(
                        3140,
                        "22032",
                        format!("Invalid JSON text: \"{e}\" in column '{column}'"),
                    ));
                }
                Value::Json(text)
            }
            _ => Value::Text(value.as_text()),
        })
    }
}

fn int_type(name: &str, display: Option<u64>, unsigned: bool) -> SqlType {
    let declared = match display {
        Some(n) => format!("{name}({n})"),
        None => name.to_owned(),
    };
    SqlType {
        kind: TypeKind::Int,
        declared,
        length: display,
        scale: None,
        unsigned,
        variants: Vec::new(),
    }
}

fn decimal_type(info: &sqlparser::ast::ExactNumberInfo) -> SqlType {
    use sqlparser::ast::ExactNumberInfo as E;
    let (precision, scale) = match info {
        E::None => (None, None),
        E::Precision(p) => (Some(*p), None),
        // sqlparser models scale as signed; MySQL scales are non-negative.
        E::PrecisionAndScale(p, s) => (Some(*p), Some((*s).max(0) as u64)),
    };
    let declared = match (precision, scale) {
        (Some(p), Some(s)) => format!("decimal({p},{s})"),
        (Some(p), None) => format!("decimal({p})"),
        _ => "decimal".to_owned(),
    };
    SqlType {
        kind: TypeKind::Decimal,
        declared,
        length: precision,
        scale,
        unsigned: false,
        variants: Vec::new(),
    }
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
