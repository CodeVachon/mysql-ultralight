//! The SQL type system, and coercion of values into declared column types.

use mysql_ultralight_protocol::{ColumnType, MemberList};
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
    /// Whether the column's text sorts and compares as bytes.
    ///
    /// MySQL calls such a column binary on the wire — a `_bin` collation sets
    /// BINARY_FLAG the same way the binary character set does — while still
    /// sending the text in the connection's character set. It is the only way
    /// a `MYSQL_TYPE_BLOB` carrying characters can carry that flag.
    ///
    /// A DDL `COLLATE` clause does not set it, and cannot: `collation::named`
    /// takes only a `_ci` collation and refuses the rest, rather than
    /// recording a name and then comparing the column the way it compares
    /// every other one. `collation::BINARY` is the name that goes with this
    /// flag.
    /// `information_schema` sets it: MySQL declares `COLUMN_TYPE`,
    /// `COLUMN_DEFAULT` and their neighbours `utf8mb3_bin`, and a client
    /// reading those columns is told they are binary.
    pub binary_collation: bool,
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
            binary_collation: false,
        }
    }

    /// Maps a parsed type to the engine's representation.
    ///
    /// Not a pure function of the tree: `REAL` is `DOUBLE` or `FLOAT`
    /// depending on `REAL_AS_FLOAT`, which is read from the statement scope
    /// rather than passed in, by `real_name` below.
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
            // `FLOAT4` is unconditionally the narrow one. Under REAL_AS_FLOAT
            // the synonym moves: MySQL then stores `REAL` in four bytes and
            // reports it as `float`, verified against 8.3 both ways.
            D::Float4 => Self::simple(TypeKind::Float, "float"),
            D::Real => Self::simple(TypeKind::Float, real_name()),
            D::RealUnsigned => {
                float_type(real_name(), &sqlparser::ast::ExactNumberInfo::None, true)
            }
            // `DOUBLE PRECISION` is not a synonym for `REAL` and the flag does
            // not reach it: MySQL leaves it eight bytes under either mode.
            D::DoublePrecisionUnsigned => {
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
                    // Unsigned, which MySQL reports on the wire and not in
                    // `COLUMN_TYPE`: a YEAR column is described `UNSIGNED
                    // ZEROFILL` there while `information_schema` says plain
                    // `year`. `simple` leaves `declared` alone, so setting the
                    // flag here changes the one without touching the other.
                    Self {
                        unsigned: true,
                        ..Self::simple(TypeKind::Year, "year")
                    }
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

    /// The type a column of pure NULLs takes on once it is materialised.
    ///
    /// `MYSQL_TYPE_NULL` is the absence of a type, and a temporary table has
    /// no field that can hold one, so MySQL substitutes the narrowest thing
    /// that can: `binary(0)`. That is not a guess — it is what
    /// `CREATE TABLE t AS SELECT NULL` writes into the table it creates, and
    /// `MYSQL_TYPE_STRING` with the binary charset and length 0 is the same
    /// column on the wire.
    ///
    /// Reached from the two places this engine materialises a result: a set
    /// operation answered at the `query` boundary, and a derived table, CTE,
    /// view or table value constructor fed back in as a relation, which all
    /// read their columns through `from_column_type`.
    ///
    /// A scalar subquery reaches it through the first of those, and only
    /// because it is described at the `query` boundary rather than at
    /// `query_with_outer`'s: `SELECT (SELECT NULL UNION SELECT NULL)` is
    /// `binary(0)` and `SELECT (SELECT NULL)` is `MYSQL_TYPE_NULL`, which is
    /// what 8.3 answers for both. That was #0104 — `computed_column_type` had
    /// no arm for a subquery and fell to its string catch-all — and the arm it
    /// has now hands back whatever the inner query was described with,
    /// materialised or not.
    pub fn materialised_null() -> Self {
        Self {
            kind: TypeKind::Blob,
            declared: "binary(0)".to_owned(),
            length: Some(0),
            scale: None,
            unsigned: false,
            variants: Vec::new(),
            binary_collation: false,
        }
    }

    /// The type a computed column of *bytes* widens to when a relation is
    /// built from it.
    ///
    /// The binary counterpart of the `varchar(512)` catch-all in
    /// [`Self::from_column_type`], and it has to be a type rather than a flag
    /// because a derived table, a CTE, a view and a `CREATE TABLE ... AS
    /// SELECT` all rebuild their columns from an `SqlType` and nothing else.
    /// MySQL models it the same way — a view over `UNHEX('4142')` is declared
    /// `varbinary(8)` on 8.3 — and `varbinary` is the spelling that keeps the
    /// wire type a `VAR_STRING`, where `binary` would make it a `STRING` and
    /// `blob` a `BLOB`.
    pub fn widened_binary() -> Self {
        Self {
            length: Some(512),
            ..Self::simple(TypeKind::Blob, "varbinary(512)")
        }
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
            binary_collation: false,
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
            ColumnType::Year => SqlType {
                unsigned: true,
                ..simple(TypeKind::Year, "year")
            },
            ColumnType::Json => simple(TypeKind::Json, "json"),
            // Reached whenever a relation is built from a result set that had
            // nothing but NULLs in a column — a derived table, a CTE, or a
            // view. Without this arm it fell to the `varchar(512)` catch-all,
            // and `SELECT * FROM (SELECT NULL) d` came back VAR_STRING where
            // MySQL, having put the column in a temporary table, says
            // `binary(0)`.
            ColumnType::Null => Self::materialised_null(),
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

    /// The `column_length` a result set reports for a column of this type.
    ///
    /// MySQL sends the *declaration's* width, and it sends it in bytes of the
    /// character set the result is being returned in — so a `VARCHAR(10)` is
    /// 40 under utf8mb4 and not 10, and a `latin1` column read back over a
    /// utf8mb4 connection is 40 too, because the server converts it on the way
    /// out. This engine speaks utf8mb4 and nothing else, so the multiplier is
    /// a constant here; it stops being one the day a second charset exists.
    ///
    /// Every number below is measured against 8.3 rather than derived, and the
    /// three that look arbitrary are the ones worth keeping: a `LONGTEXT` is
    /// 4294967295 and not 4×4294967295, because the width saturates at what
    /// four bytes hold; a `DECIMAL`'s length counts the point and the sign as
    /// well as the digits; and a `TINYINT UNSIGNED` is 3 against a signed 4,
    /// because the sign is a column of display width.
    pub fn wire_length(&self) -> u32 {
        /// utf8mb4, the only character set this engine has.
        const BYTES_PER_CHAR: u64 = 4;
        // In characters, then multiplied — the saturation is at the end, so a
        // LONGTEXT lands on u32::MAX rather than wrapping through it.
        let chars = |n: u64| u32::try_from(n.saturating_mul(BYTES_PER_CHAR)).unwrap_or(u32::MAX);
        let bytes = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
        // A temporal's fractional seconds are parsed into `length`, not
        // `scale`: `datetime(3)` is 19 + the point + three digits.
        let fractional = |base: u64| match self.length {
            Some(precision) if precision > 0 => base + 1 + precision,
            _ => base,
        };
        match self.kind {
            // `TINYINT(1)`, and the only integer whose declared width MySQL
            // still reports in `COLUMN_TYPE` as well as on the wire.
            TypeKind::Bool => 1,
            TypeKind::Int => bytes(self.length.unwrap_or_else(|| {
                let widest = match self.base_name().as_str() {
                    "tinyint" => 4,
                    "smallint" => 6,
                    "mediumint" => 9,
                    "bigint" => 20,
                    _ => 11,
                };
                // One narrower where there is no sign to print. A BIGINT is
                // the exception both ways: 20 digits is what its unsigned
                // range needs, so there is no sign column to give back.
                if self.unsigned && widest != 20 {
                    widest - 1
                } else {
                    widest
                }
            })),
            // A declared `(M,D)` reports M. Without one MySQL reports the
            // widest value the type can print: 12 for four bytes, 22 for eight.
            TypeKind::Float => bytes(self.length.unwrap_or({
                if self.base_name() == "float" {
                    12
                } else {
                    22
                }
            })),
            // The digits, plus a place for the decimal point where there is a
            // scale, plus a place for the sign where the column can be
            // negative. `DECIMAL(10,2)` is 12, `DECIMAL(10,0)` is 11 and
            // `DECIMAL(8,3) UNSIGNED` is 9.
            TypeKind::Decimal => {
                let precision = self.length.unwrap_or(10);
                let point = u64::from(self.scale.unwrap_or(0) > 0);
                let sign = u64::from(!self.unsigned);
                bytes(precision + point + sign)
            }
            TypeKind::Text => match self.base_name().as_str() {
                "tinytext" => chars(255),
                "text" => chars(65535),
                "mediumtext" => chars(16777215),
                "longtext" => chars(4294967295),
                // A bare `CHAR` is `CHAR(1)`.
                _ => chars(self.length.unwrap_or(1)),
            },
            // Bytes, so no multiplier: the declared width of a `VARBINARY(9)`
            // is nine bytes however many characters they would make.
            TypeKind::Blob => match self.base_name().as_str() {
                "tinyblob" => 255,
                "blob" => 65535,
                "mediumblob" => 16777215,
                "longblob" => u32::MAX,
                _ => bytes(self.length.unwrap_or(1)),
            },
            TypeKind::Date => 10,
            // `datetime` and `timestamp` both print 19 characters before the
            // fraction; `time` prints `[-]HHH:MM:SS`, which is 10.
            TypeKind::DateTime => bytes(fractional(19)),
            TypeKind::Time => bytes(fractional(10)),
            TypeKind::Year => 4,
            TypeKind::Json => u32::MAX,
            // The widest member, which is what a value of the column can be.
            TypeKind::Enum => chars(
                self.variants
                    .iter()
                    .map(|v| v.chars().count() as u64)
                    .max()
                    .unwrap_or(0),
            ),
            // Every member at once, separated by commas, which is what a value
            // of the column can be: `SET('a','bb','ccc')` is `a,bb,ccc`.
            TypeKind::Set => chars(
                self.variants
                    .iter()
                    .map(|v| v.chars().count() as u64)
                    .sum::<u64>()
                    + self.variants.len().saturating_sub(1) as u64,
            ),
        }
    }

    /// Whether the column is declared as a list of members, and which kind.
    ///
    /// Separate from `column_type`, because that answer is `STRING` for both
    /// and for a `CHAR(n)` besides: the flag is the only place the difference
    /// survives to the wire.
    pub fn member_list(&self) -> Option<MemberList> {
        match self.kind {
            TypeKind::Enum => Some(MemberList::Enum),
            TypeKind::Set => Some(MemberList::Set),
            _ => None,
        }
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
            // A `FLOAT` says so, which is what makes the binary protocol send
            // four bytes rather than eight. It could not until the value was
            // four bytes too, which is why this read `Double` before.
            TypeKind::Float if self.base_name() == "float" => ColumnType::Float,
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

    /// The limits MySQL puts on a declared `(M,D)`, in the order it checks
    /// them.
    ///
    /// Not cosmetic. `M` and `D` reach here as `u64` — the parser takes any
    /// number the grammar allows — and both the fixed-scale rendering and a
    /// DECIMAL's own storage build a string with `D` digits after the point,
    /// so `DECIMAL(10,1000000)` writes a million-character value and
    /// `DOUBLE(10,4294967295)` asks for four billion. One statement, and no
    /// value in the row large enough to hint at it. MySQL refuses all of them
    /// at `CREATE TABLE`.
    ///
    /// The order is measured: `FLOAT(300,31)` is 1425 and not 1439, and
    /// `DECIMAL(66,31)` is 1425 and not 1426, so the scale is checked first.
    /// `FLOAT(2,5)` is 1427, which only a declaration that passed both limits
    /// can reach.
    ///
    /// Columns only. A cast declares a type too and MySQL polices it by
    /// different rules — `CAST(1.5 AS DECIMAL(10,31))` is 1427 there where the
    /// column is 1425 — so this is called where a column is built rather than
    /// where a type is parsed.
    pub fn check_declaration_limits(&self, column: &str) -> EngineResult<()> {
        const MAX_SCALE: u64 = 30;
        if !matches!(self.kind, TypeKind::Float | TypeKind::Decimal) {
            return Ok(());
        }
        if let Some(scale) = self.scale.filter(|scale| *scale > MAX_SCALE) {
            return Err(EngineError::new(
                1425,
                "42000",
                format!(
                    "Too big scale {scale} specified for column '{column}'. Maximum is {MAX_SCALE}."
                ),
            ));
        }
        // A DECIMAL counts 65 digits and reports going past them with its own
        // code; the two approximate types are measured in display width
        // instead and stop at 255.
        let decimal = self.kind == TypeKind::Decimal;
        let max_precision = if decimal { 65 } else { 255 };
        if let Some(length) = self.length.filter(|length| *length > max_precision) {
            return Err(if decimal {
                EngineError::new(
                    1426,
                    "42000",
                    format!(
                        "Too-big precision {length} specified for '{column}'. \
                         Maximum is {max_precision}."
                    ),
                )
            } else {
                EngineError::new(
                    1439,
                    "42000",
                    format!(
                        "Display width out of range for column '{column}' (max = {max_precision})"
                    ),
                )
            });
        }
        // Only where both were written: a lone `(M)` has no scale to outgrow,
        // and `DECIMAL(8)` is `DECIMAL(8,0)` rather than an error.
        if let (Some(length), Some(scale)) = (self.length, self.scale) {
            if length < scale {
                return Err(EngineError::new(
                    1427,
                    "42000",
                    format!(
                        "For float(M,D), double(M,D) or decimal(M,D), \
                         M must be >= D (column '{column}')."
                    ),
                ));
            }
        }
        Ok(())
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
            TypeKind::Float if self.base_name() == "float" => Value::Float32(0.0),
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
            //
            // Tagged, like the decimal above and for the same reason: a row
            // filled from its default holds a temporal like every other row,
            // and left as text it would be the one DATE in the column that
            // reads 1970 in arithmetic instead of 19700101.
            TypeKind::Date => Value::Temporal("1970-01-01".to_owned()),
            TypeKind::DateTime => Value::Temporal("1970-01-01 00:00:00".to_owned()),
            TypeKind::Time => Value::Temporal("00:00:00".to_owned()),
            // Tagged, so a column filled from its default holds a document
            // like every other row does.
            TypeKind::Json => Value::Json("null".to_owned()),
            // An ENUM's implicit default is its first member, not an empty
            // string, which would not be a legal value at all.
            //
            // Tagged, like the temporal and the decimal above: a row filled
            // from its default holds an ENUM like every other row, and left as
            // text it would be the one member in the column that reads 0 in
            // arithmetic instead of 1.
            TypeKind::Enum => match self.variants.first() {
                Some(first) => Value::Enum {
                    member: first.clone(),
                    ordinal: 1,
                },
                None => Value::Enum {
                    member: String::new(),
                    ordinal: 0,
                },
            },
            // Empty *bytes*, not empty text: everything that reads a binary
            // column dispatches on the variant, so a default-filled row must
            // carry the same one an inserted row does. It did not, and the
            // same column answered two ways — after `ALTER TABLE ... ADD
            // COLUMN b VARBINARY(8) NOT NULL` backfilled an existing row,
            // `b & 1` gave 0 for that row and 1235 for a normally inserted
            // one, because the bitwise operators refuse a binary operand and
            // this default was not one.
            TypeKind::Blob => Value::Bytes(Vec::new()),
            TypeKind::Text | TypeKind::Set => Value::text(""),
        }
    }

    /// The values this integer column can hold.
    ///
    /// MySQL sizes an integer by its declared width and refuses anything
    /// outside it, which is not a formality here: the binary protocol encodes
    /// by the declared type, so a `TINYINT` that had been allowed to hold 999
    /// would reach a client as 231.
    fn integer_range(&self) -> (i128, i128) {
        let (signed_low, signed_high, unsigned_high) = match self.base_name().as_str() {
            "tinyint" => (-128, 127, 255),
            "smallint" => (-32_768, 32_767, 65_535),
            "mediumint" => (-8_388_608, 8_388_607, 16_777_215),
            "int" | "integer" => (-2_147_483_648, 2_147_483_647, 4_294_967_295),
            // `i128` throughout so this row can say what it means: a
            // `BIGINT UNSIGNED` holds 18446744073709551615 in MySQL, and while
            // the engine held only an `i64` this had to claim `i64::MAX` and
            // refuse the top half of the column's own range.
            _ => (
                i128::from(i64::MIN),
                i128::from(i64::MAX),
                i128::from(u64::MAX),
            ),
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
                let n = value.as_exact_int().ok_or_else(mismatch)?;
                let (low, high) = self.integer_range();
                if n < low || n > high {
                    return Err(out_of_range());
                }
                // Tagged here because this is the last place the column's own
                // signedness and the value are both in hand — the same
                // argument the ENUM ordinal below makes. An operand being
                // subtracted from something has no column left to ask, and
                // whether `iu - 100` is 1690 or -93 is entirely the column's
                // answer. See #0069.
                if self.unsigned {
                    Value::Unsigned(n as u64)
                } else {
                    Value::Int(n as i64)
                }
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
                // The two-argument form is not a display rule. MySQL *stores*
                // a `FLOAT(M,D)` or `DOUBLE(M,D)` rounded to D places, which
                // is visible far from any column list: `JSON_ARRAY(f)` over a
                // `FLOAT(10,4)` given 1.234567890123 is
                // `[1.2345999479293823]` on 8.3 — the four bytes of 1.2346 —
                // where the unrounded value would be `[1.2345678806304932]`,
                // and the binary protocol sends those same bytes.
                //
                // Rounded before the narrowing, not after, which is measured:
                // 2.00005 into a `FLOAT(10,4)` is 2.0000 there. Its `f64` is
                // just below the half and its `f32` just above, so narrowing
                // first would round the other way.
                let n = match self.scale {
                    Some(scale) => {
                        let rounded = rounded_to_scale(n, scale);
                        // The range is what the digits allow rather than what
                        // the type can hold: 9999.995 into a `FLOAT(6,2)` is
                        // 1264 because it *rounds* to 10000.00, while 9999.994
                        // is taken. So the check follows the rounding.
                        if !fits_digits(rounded, self.length, scale) {
                            return Err(out_of_range());
                        }
                        rounded
                    }
                    None => n,
                };
                // A `FLOAT` is four bytes in MySQL and holds what four bytes
                // hold: 1.234567890123 comes back 1.23457. Keeping the double
                // made the column a `DOUBLE` that happened to be declared
                // narrower.
                if self.base_name() == "float" {
                    let narrowed = n as f32;
                    // `as f32` saturates rather than failing, so 3.5e38 became
                    // an infinity and the column answered the token `Inf` —
                    // not a number any client reads back. MySQL refuses the
                    // statement, which is the only answer that stays true.
                    if narrowed.is_infinite() && n.is_finite() {
                        return Err(out_of_range());
                    }
                    Value::Float32(narrowed)
                } else {
                    Value::Float(n)
                }
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
                Value::Decimal(decimal_digits(&stored, self.scale.unwrap_or(0) as usize))
            }
            TypeKind::Blob => match value {
                Value::Bytes(b) => Value::Bytes(b),
                other => Value::Bytes(other.as_text().into_bytes()),
            },
            // Tagged with the position it was declared at, which is the
            // number MySQL reads an ENUM as everywhere but a string context.
            // `variants` is in declaration order and this is the last place it
            // and the value are both in hand — an operand being added to
            // something has no column left to ask.
            //
            // Assignment reads the same way round, which is measured: a member
            // wins over an index, so `'2'` into `ENUM('2','1')` is the *first*
            // member and not the second, while `'2'` into `ENUM('a','b','c')`
            // is `b`. The match is case-insensitive and stores the declared
            // spelling, so `'A'` into `ENUM('a')` reads back `a`.
            TypeKind::Enum => {
                // A *number* is only ever an index, and never a member: `2`
                // into `ENUM('2','1')` is the second member, `1`, and not the
                // one whose text is `2`. It truncates towards zero rather than
                // rounding, so 1.6 is still the first member and 0.4 is
                // refused with the number 0.
                let numeric = match &value {
                    // A boolean is a number here, which is measured: `TRUE`
                    // into `ENUM('2','1')` stores the *first* member, `2`,
                    // exactly as the literal 1 does — where reading it as the
                    // text `1` would have matched the member `1` instead.
                    Value::Int(n) => Some(*n),
                    Value::Bool(b) => Some(i64::from(*b)),
                    // An unsigned integer is a number here too, and for the
                    // same reason: MySQL reads a number as an index whatever
                    // its signedness. Left out, `1 & 1` into `ENUM('2','1')`
                    // was read as the *text* `1` and matched the member `1`,
                    // where the plain literal 1 beside it stored the first
                    // member `2` — the same number stored two ways, decided by
                    // a tag the statement never mentioned. An index above
                    // `i64::MAX` wraps negative and is refused, which is the
                    // right answer for a position no column declares.
                    Value::Unsigned(n) => Some(*n as i64),
                    Value::Float(_) | Value::Float32(_) | Value::Decimal(_) => {
                        Some(value.as_number().trunc() as i64)
                    }
                    _ => None,
                };
                let text = value.as_text();
                // The error member copied out of another ENUM. It is a legal
                // stored value — `'0'` writes one — and MySQL carries it
                // across: `UPDATE t SET g = g` and `INSERT ... SELECT` both
                // keep it, and so does a copy into a column declared with
                // different members. Nothing else may store an empty member,
                // because `g = ''` is 1265 for a column that does not declare
                // one, so the tag is what tells the two apart.
                if matches!(&value, Value::Enum { ordinal: 0, .. }) {
                    return Ok(Value::Enum {
                        member: String::new(),
                        ordinal: 0,
                    });
                }
                let member = |index: usize| Value::Enum {
                    member: self.variants[index].clone(),
                    ordinal: index as u32 + 1,
                };
                let by_index = |n: i64| match n {
                    n if n >= 1 && (n as usize) <= self.variants.len() => {
                        Some(member(n as usize - 1))
                    }
                    _ => None,
                };
                let stored = match numeric {
                    Some(n) => by_index(n),
                    None => self
                        .variants
                        .iter()
                        .position(|v| crate::value::collate_eq(v, &text))
                        .map(member)
                        // Not a member, so a string is read as an index — but
                        // only when it is a whole number, so `'2'` is the
                        // second member where `'1.0'` and `'2abc'` are errors.
                        .or_else(|| {
                            let n = text.trim().parse::<i64>().ok()?;
                            // Index 0 is MySQL's error member, the empty
                            // string, and only a string reaches it: `'0'`
                            // stores it where the number 0 above is refused.
                            // Measured, and not a rule the rest of this would
                            // predict.
                            if n == 0 {
                                return Some(Value::Enum {
                                    member: String::new(),
                                    ordinal: 0,
                                });
                            }
                            by_index(n)
                        }),
                };
                match stored {
                    Some(value) => value,
                    None => {
                        return Err(EngineError::new(
                            1265,
                            "01000",
                            format!("Data truncated for column '{column}'"),
                        ))
                    }
                }
            }
            // A JSON column holds a document, so reading one and passing it to
            // a JSON function embeds it as a document. MySQL validates on the
            // way in, and so does this: a column that accepted `{"a":1` would
            // fail every later read of it instead, far from the cause.
            TypeKind::Json => {
                // Through `document_of`, so a document written by a
                // constructor keeps its numbers as they were built and only
                // *text* is re-read the way MySQL's parser reads it. Storing
                // `JSON_OBJECT('a', d)` over a DECIMAL(10,2) keeps the scale;
                // storing the string `'{"a": 2.50}'` does not, which is what
                // 8.3 does with each. See #0071.
                //
                // Serde's own message, because "invalid" without a position is
                // no help against a document of any size — and the sibling
                // 3141 raised by `json::parse` already says where.
                let parsed = crate::json::document_of(&value).map_err(|e| {
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
            // Tagged, so the column's declared type reaches the evaluator on
            // the value. Text is all the engine stores either way — MySQL
            // transmits a temporal as text and so does this — but a DATE and a
            // VARCHAR holding the same characters answer differently in every
            // numeric context, and the tag is the only thing that says which
            // is which. TIMESTAMP is a `DateTime` here, so it is covered too.
            //
            // Written into the column's own shape rather than kept as typed,
            // because MySQL stores a temporal by its type: a DATETIME given
            // `'2024-01-05'` holds `2024-01-05 00:00:00` and a TIME(0) given
            // `'06:07:08.567'` holds `06:07:09`. Keeping the characters made
            // the packed number a property of the statement — and left a
            // fraction under a column typed LONGLONG, which the two protocols
            // cannot both send. Text that is not a temporal at all is kept as
            // written, which is what this column has always done with it.
            TypeKind::Date | TypeKind::DateTime | TypeKind::Time => {
                let text = value.as_text();
                let shape = match self.kind {
                    TypeKind::Date => crate::temporal::Shape::Date,
                    TypeKind::Time => crate::temporal::Shape::Time,
                    _ => crate::temporal::Shape::DateTime,
                };
                let precision = self.length.unwrap_or(0) as u32;
                Value::Temporal(crate::temporal::normalise(&text, shape, precision).unwrap_or(text))
            }
            _ => Value::Text(value.as_text()),
        })
    }
}

/// `text` as a DECIMAL of `scale` digits after the point.
///
/// The same conversion on the way into a column and at a `CAST`, because MySQL
/// applies the same one: `CAST('3.75' AS DECIMAL(10,1))` is 3.8 there exactly
/// as a `DECIMAL(10,1)` column stores 3.75 as 3.8.
///
/// A decimal is rescaled exactly however many digits it has: the counted path
/// first, and the one that works on the characters where an `i128` runs out —
/// which a `DECIMAL(40,30)` reaches at ten integer digits, well inside what
/// MySQL holds. Only text that is not a decimal at all becomes the number
/// MySQL reads out of it, which is 0 for a name and 1000 for `'1e3'` — neither
/// of them text a client could read back under a NEWDECIMAL.
pub(crate) fn decimal_digits(text: &str, scale: usize) -> String {
    let rescaled = |text: &str| {
        crate::decimal::rescale(text, scale).or_else(|| crate::decimal::rescale_wide(text, scale))
    };
    rescaled(text)
        // An exponent is spelled out first rather than folded through an
        // `f64`: `CAST('1e3' AS DECIMAL(65,30))` came back
        // 1000.000000000000089690419062898688 that way, where MySQL answers
        // thirty zeros, and a `DECIMAL(65,30)` column storing `'1e3'` did the
        // same. The float only ever held about fifteen of those digits.
        .or_else(|| crate::decimal::expanded(text).as_deref().and_then(rescaled))
        .unwrap_or_else(|| crate::decimal::from_f64(crate::value::numeric_prefix(text), scale))
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
        binary_collation: false,
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
        binary_collation: false,
    }
}

/// What `REAL` declares, which `sql_mode` decides rather than the grammar.
///
/// MySQL's default is `DOUBLE`; `REAL_AS_FLOAT` makes it `FLOAT`, and the
/// declared name is what everything downstream reads — the storage width, the
/// wire type code, and what `information_schema` reports.
fn real_name() -> &'static str {
    if crate::exec::real_as_float() {
        "float"
    } else {
        "double"
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
        binary_collation: false,
    }
}

/// A double rounded to `scale` decimal places, the way MySQL rounds one into a
/// `FLOAT(M,D)` or `DOUBLE(M,D)` column.
///
/// Ties go to the even digit, which is `rint`'s rule and not `round`'s, and is
/// measured rather than assumed: into a `FLOAT(10,2)`, 0.125 stores 0.12 where
/// half-away-from-zero would give 0.13, 0.015 stores 0.02 and 0.025 stores
/// 0.02. All three are exact halves *after* the multiply, so nothing but the
/// tie rule separates them. The neighbouring 0.135 and 0.145 are not halves at
/// all — their doubles fall just above and just below — and they store 0.14
/// either way, which is why they are useless as evidence and the three above
/// are not.
///
/// The multiply is the same one MySQL does, so a scale that overflows to an
/// infinity leaves the value alone rather than turning it into one.
fn rounded_to_scale(value: f64, scale: u64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let factor = power_of_ten(scale);
    let scaled = value * factor;
    if !scaled.is_finite() {
        return value;
    }
    let rounded = scaled.round_ties_even();
    // Nothing to round, so nothing to divide. The division is not free: it is
    // a second rounding, and at a wide scale it moves a value that was already
    // exact. 1.5 into a `DOUBLE(255,30)` multiplies to 1.5e30, which is
    // integral and comes back unchanged — and then `1.5e30 / 1e30` is
    // 1.5000000000000002, because neither power of ten is exact. MySQL sends
    // 1.5 for that row, and so does this.
    let result = if rounded == scaled {
        value
    } else {
        rounded / factor
    };
    // MySQL stores the positive zero. -0.001 into a `FLOAT(10,2)` rounds to a
    // *negative* zero, which reads back `-0.00`, embeds in a document as
    // `-0.0` and reaches a binary client with its sign bit set; 8.3 answers
    // `0.00`, `0.0` and a plain zero for all three. Normalised here rather
    // than in the rendering because all three readings are wrong, not just the
    // printed one — and only the rounding can produce it, which is why the
    // sign survives on -0.006 rounding to -0.01 beside it.
    if result == 0.0 {
        return 0.0;
    }
    result
}

/// Ten to the power of `exponent`, written out the way MySQL's own `log_10[]`
/// table is.
///
/// `10f64.powi(n)` agrees with every entry here on this host, so this fixes no
/// measured bug and no test tells the two apart. It is written out because
/// `powi` is a binary exponentiation and nothing promises it is correctly
/// rounded: above 22 an f64 no longer holds a power of ten exactly, and a
/// value that is an exact half against the true power is not one against a
/// neighbour an ulp away — which is the one thing the tie rule above cannot
/// survive. A literal is the same constant on every host.
///
/// Thirty-one entries because that is every scale a column may declare;
/// `check_declaration_limits` refuses the rest.
fn power_of_ten(exponent: u64) -> f64 {
    const POWERS: [f64; 31] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
        1e17, 1e18, 1e19, 1e20, 1e21, 1e22, 1e23, 1e24, 1e25, 1e26, 1e27, 1e28, 1e29, 1e30,
    ];
    match POWERS.get(exponent as usize) {
        Some(power) => *power,
        // Only the digit check below reaches past thirty, and it compares
        // rather than rounds: an exponent that overflows to an infinity fails
        // every comparison a finite value can make, which is the right answer
        // for a width nothing can outgrow.
        None => 10f64.powi(exponent.min(i32::MAX as u64) as i32),
    }
}

/// Whether a rounded value fits the `M` digits a `FLOAT(M,D)` declares.
///
/// `M` counts every digit and `D` of them are after the point, so the value
/// has to stay below 10 to the power of the difference. MySQL raises 1264
/// rather than truncating, and it does so on the *rounded* value.
fn fits_digits(value: f64, precision: Option<u64>, scale: u64) -> bool {
    let Some(precision) = precision else {
        return true;
    };
    // Unreachable from a column, since `check_declaration_limits` answers
    // 1427 for `M < D` before a value can be stored in one. Kept so this stays
    // total rather than relying on that from a distance.
    let Some(integral) = precision.checked_sub(scale) else {
        return false;
    };
    value.abs() < power_of_ten(integral)
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
        binary_collation: false,
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
        binary_collation: false,
    }
}

fn character_len(length: &sqlparser::ast::CharacterLength) -> u64 {
    use sqlparser::ast::CharacterLength as C;
    match length {
        C::IntegerLength { length, .. } => *length,
        C::Max => 0,
    }
}
