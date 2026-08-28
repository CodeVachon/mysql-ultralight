//! Column definitions and MySQL type codes.

use crate::buf::BufWriter;

/// MySQL column type codes, as they appear in result set metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ColumnType {
    Decimal = 0x00,
    Tiny = 0x01,
    Short = 0x02,
    Long = 0x03,
    Float = 0x04,
    Double = 0x05,
    Null = 0x06,
    Timestamp = 0x07,
    LongLong = 0x08,
    Int24 = 0x09,
    Date = 0x0a,
    Time = 0x0b,
    DateTime = 0x0c,
    Year = 0x0d,
    VarChar = 0x0f,
    Bit = 0x10,
    Json = 0xf5,
    NewDecimal = 0xf6,
    Enum = 0xf7,
    Set = 0xf8,
    TinyBlob = 0xf9,
    MediumBlob = 0xfa,
    LongBlob = 0xfb,
    Blob = 0xfc,
    VarString = 0xfd,
    String = 0xfe,
    Geometry = 0xff,
}

impl ColumnType {
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// Column definition flags.
pub mod flags {
    pub const NOT_NULL: u16 = 0x0001;
    pub const PRI_KEY: u16 = 0x0002;
    pub const UNIQUE_KEY: u16 = 0x0004;
    pub const MULTIPLE_KEY: u16 = 0x0008;
    pub const BLOB: u16 = 0x0010;
    pub const UNSIGNED: u16 = 0x0020;
    pub const ZEROFILL: u16 = 0x0040;
    pub const BINARY: u16 = 0x0080;
    pub const ENUM: u16 = 0x0100;
    pub const AUTO_INCREMENT: u16 = 0x0200;
    pub const TIMESTAMP: u16 = 0x0400;
    pub const SET: u16 = 0x0800;
    /// Set by MySQL on a column a temporary table produced, and on an ENUM.
    /// Nothing here sends it: it was on every numeric column until the
    /// differential harness compared the flags word and MySQL turned out not
    /// to send it for one at all — an `INT` column is `flags=0` on 8.3.
    pub const NUM: u16 = 0x8000;
}

/// Character set ids. `utf8mb4_0900_ai_ci` is the MySQL 8 default; `binary`
/// marks columns whose bytes must not be reinterpreted.
pub mod charset {
    pub const BINARY: u16 = 63;
    pub const UTF8MB4_GENERAL_CI: u16 = 45;
    pub const UTF8MB4_0900_AI_CI: u16 = 255;
}

/// One column in a result set.
///
/// Compared field by field to decide whether a prepared statement's cached
/// metadata has gone stale, so a change to any of it counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub schema: String,
    pub table: String,
    pub org_table: String,
    pub name: String,
    pub org_name: String,
    pub charset: u16,
    pub column_length: u32,
    pub column_type: ColumnType,
    pub flags: u16,
    pub decimals: u8,
}

impl ColumnDef {
    /// A minimal column: a name and a type, with sensible defaults for the
    /// metadata clients rarely inspect.
    pub fn new(name: impl Into<String>, column_type: ColumnType) -> Self {
        let name = name.into();
        let (charset, column_length, flags) = match column_type {
            ColumnType::Tiny
            | ColumnType::Short
            | ColumnType::Long
            | ColumnType::LongLong
            | ColumnType::Int24
            | ColumnType::Float
            | ColumnType::Double
            | ColumnType::Decimal
            // No NUM: MySQL does not send it for a number. See `flags::NUM`.
            | ColumnType::NewDecimal => (charset::BINARY, 20, flags::NOT_NULL),
            ColumnType::Blob
            | ColumnType::LongBlob
            | ColumnType::MediumBlob
            | ColumnType::TinyBlob => (charset::BINARY, u32::MAX, flags::BLOB),
            // A date is not text, and neither is a JSON document — MySQL
            // reports the binary collation for both. Only the string types
            // default to a character set, and a `BLOB` that turns out to be a
            // `TEXT` is corrected by `as_text`.
            ColumnType::Date
            | ColumnType::DateTime
            | ColumnType::Timestamp
            | ColumnType::Time
            | ColumnType::Json
            | ColumnType::Null => (charset::BINARY, 20, 0),
            // A year has no negative half, and MySQL says so — without the
            // flag the binary protocol reads its two bytes back signed.
            ColumnType::Year => (charset::BINARY, 20, flags::UNSIGNED),
            _ => (charset::UTF8MB4_0900_AI_CI, 4096, 0),
        };

        Self {
            schema: String::new(),
            table: String::new(),
            org_table: String::new(),
            org_name: name.clone(),
            name,
            charset,
            column_length,
            column_type,
            flags,
            decimals: 0,
        }
    }

    pub fn with_table(mut self, table: impl Into<String>) -> Self {
        let table = table.into();
        self.org_table = table.clone();
        self.table = table;
        self
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = schema.into();
        self
    }

    /// Marks the column as holding an unsigned number.
    ///
    /// The binary protocol reads a column's bytes back as a signed number
    /// unless this says otherwise, so an `INT UNSIGNED` holding three billion
    /// arrives negative without it.
    pub fn unsigned(mut self) -> Self {
        self.flags |= flags::UNSIGNED;
        self
    }

    /// Marks the column as carrying text, or as carrying bytes.
    ///
    /// The only thing on the wire that tells a `TEXT` column from a `BLOB`
    /// one: both are `MYSQL_TYPE_BLOB`, and the charset is what says which. A
    /// text column sent with the binary charset arrives as bytes — Prisma
    /// base64s it and then refuses to convert it to a String.
    pub fn as_text(mut self, textual: bool) -> Self {
        self.charset = if textual {
            charset::UTF8MB4_0900_AI_CI
        } else {
            charset::BINARY
        };
        self
    }

    pub fn with_flags(mut self, flags: u16) -> Self {
        self.flags = flags;
        self
    }

    /// The width the column was declared with, in bytes of the result's
    /// character set.
    ///
    /// Not decoration: `column_length` is what a client sizes an input widget
    /// or a generated field from, and every string column left here saying
    /// 4096 until the differential harness learned to compare it.
    pub fn with_length(mut self, column_length: u32) -> Self {
        self.column_length = column_length;
        self
    }

    /// Whether the column's values are bytes rather than characters.
    ///
    /// Sets the flag as well as clearing it, for the same reason `nullable`
    /// does: the defaults in `new` are by type code, and a column can be
    /// either side of this for one code — a `MYSQL_TYPE_STRING` is binary as a
    /// `BINARY(16)` and not as a `CHAR(16)`.
    pub fn binary(mut self, binary: bool) -> Self {
        if binary {
            self.flags |= flags::BINARY;
        } else {
            self.flags &= !flags::BINARY;
        }
        self
    }

    /// Whether the column is zero-filled to its display width.
    ///
    /// Only a stored `YEAR` is, of the types this engine has: MySQL reports it
    /// `UNSIGNED ZEROFILL` on the wire while `information_schema` says plain
    /// `year`. `YEAR(x)` computes one and is not zero-filled, which is why
    /// this is a setter rather than a default in `new`.
    pub fn zerofill(mut self, zerofill: bool) -> Self {
        if zerofill {
            self.flags |= flags::ZEROFILL;
        } else {
            self.flags &= !flags::ZEROFILL;
        }
        self
    }

    /// Whether MySQL describes the column as a blob.
    ///
    /// True of a stored `TEXT`, `BLOB` or `JSON` column and false of an
    /// expression that happens to produce one: `GROUP_CONCAT` is a
    /// `MYSQL_TYPE_LONG_BLOB` with no BLOB flag on 8.3, because the flag comes
    /// off the field a column is read from and there is no field behind it.
    pub fn blob(mut self, blob: bool) -> Self {
        if blob {
            self.flags |= flags::BLOB;
        } else {
            self.flags &= !flags::BLOB;
        }
        self
    }

    /// Records whether the column accepts NULL.
    ///
    /// Sets the flag as well as clearing it. It used to only clear, and
    /// `ColumnDef::new` only turned it on for the numeric codes — so a
    /// nullable `INT` was right by being switched off again, while a
    /// `NOT NULL VARCHAR` never had it switched on and reached the client as
    /// optional. Generated code then modelled a non-null column as one.
    pub fn nullable(mut self, nullable: bool) -> Self {
        if nullable {
            self.flags &= !flags::NOT_NULL;
        } else {
            self.flags |= flags::NOT_NULL;
        }
        self
    }

    /// Encodes a `ColumnDefinition41` packet.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = BufWriter::with_capacity(64 + self.name.len());
        w.lenenc_str("def")
            .lenenc_str(&self.schema)
            .lenenc_str(&self.table)
            .lenenc_str(&self.org_table)
            .lenenc_str(&self.name)
            .lenenc_str(&self.org_name)
            // Length of the fixed-size block that follows.
            .lenenc_int(0x0c)
            .u16(self.charset)
            .u32(self.column_length)
            .u8(self.column_type.code())
            .u16(self.flags)
            .u8(self.decimals)
            .zeros(2);
        w.into_inner()
    }
}
