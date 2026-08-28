//! The state image: the whole catalog written to one file and read back.
//!
//! This is the mechanism the engine exists for. Replaying 604 migrations costs
//! seconds even at this engine's speed, and a test suite that starts a fresh
//! process per file pays it every time. Baking the result once and loading it
//! turns that into a few milliseconds.
//!
//! The format is hand-rolled for the same reason the wire codec is: it has to
//! be exact, and a derive would hide the one decision that matters — that
//! nothing here may be reordered. Field order *is* the format. Adding a field
//! means a new [`FORMAT_VERSION`], never an insertion in the middle.

use std::collections::BTreeMap;

use crate::catalog::{Catalog, Check, Column, Database, ForeignKey, Index, Table, View};
use crate::error::{EngineError, EngineResult};
use crate::types::{SqlType, TypeKind};
use crate::value::Value;

/// Identifies the file at a glance, and stops a wrong file being read as one.
const MAGIC: &[u8; 4] = b"MULi";

/// Bumped whenever the layout below changes in any way, or whenever what a
/// field *means* changes.
///
/// A mismatch is refused rather than guessed at: reading a v1 image as v2 would
/// not fail cleanly, it would silently produce a catalog with the wrong shape,
/// and every test running against it would be wrong in a way nothing reports.
///
/// v9 adds a field to a column's type: whether its text sorts as bytes. A
/// `_bin` collation is what puts the BINARY flag on a text column's wire
/// definition, and `CREATE TABLE t AS SELECT COLUMN_TYPE FROM
/// information_schema.columns` puts such a column into a user table — 8.3
/// declares that copy `utf8mb3_bin` too. A v8 image would reload it without
/// the flag, so the same table would describe itself one way before a snapshot
/// and another way after it, which is the class of silent difference this
/// version check exists to stop.
///
/// v8 adds another: an UNSIGNED integer. A v7 image cannot hold one at all —
/// its widest tag is an `i64`, so a `BIGINT UNSIGNED` column was capped at
/// `i64::MAX` and 18446744073709551615 could not be stored — and a v7 image
/// that *could* be read as v8 would reload every unsigned column signed, which
/// decides whether `iu - 100` is 1690 or -93. See #0069.
///
/// v7 adds another: an ENUM member and the ordinal it was declared at, which a
/// v6 image would reload as text — and a member read as text is 0 in
/// arithmetic where MySQL says its position. The ordinal cannot be recovered
/// on the way in either, because a value is loaded before anything asks which
/// column it belongs to.
///
/// v6 adds another: a temporal, which a v5 image would reload as text — and a
/// DATE read as text is 2024 in arithmetic where MySQL says 20240105.
///
/// v5 adds another: a four-byte float, which a v4 image would reload as a
/// double and print to seventeen digits where MySQL shows six.
///
/// v4 adds a value tag: a decimal is written as its own kind rather than
/// as text, because the two are compared differently — by value and by
/// characters — and a v3 image would reload every decimal as a string.
///
/// v3 is a meaning change, not a layout one. `SqlType::declared` now holds what
/// MySQL reports as `COLUMN_TYPE` — `bigint unsigned` where a v2 image says
/// `bigint`, `int` where it says `int(11)`, `set('x','y')` where it says
/// `set(x,y)`. A v2 image parses cleanly as v3 and would answer
/// `information_schema` exactly the way this build was written to stop
/// answering it, which is the wrong answer the version check exists to
/// prevent. An image is a file a user keeps and boots from with `--state`, so
/// the stale one has to fail rather than mislead.
pub const FORMAT_VERSION: u32 = 9;

/// Writes the whole catalog.
pub fn encode(catalog: &Catalog) -> Vec<u8> {
    let mut w = Writer::new();
    w.bytes(MAGIC);
    w.u32(FORMAT_VERSION);

    let databases = catalog.all_databases();
    w.u32(databases.len() as u32);
    for (key, database) in databases {
        w.str(key);
        write_database(&mut w, database);
    }
    w.finish()
}

/// Reads a catalog back.
///
/// Every read is bounds-checked: a truncated or corrupt image has to be an
/// error, because the alternative is a process that starts with a catalog
/// missing tables nobody notices until a test fails somewhere unrelated.
pub fn decode(bytes: &[u8]) -> EngineResult<Catalog> {
    let mut r = Reader::new(bytes);

    let magic = r.bytes(4)?;
    if magic != MAGIC {
        return Err(malformed("not a mysql-ultralight state image"));
    }
    let version = r.u32()?;
    if version != FORMAT_VERSION {
        return Err(malformed(format!(
            "state image is format version {version}, this build reads {FORMAT_VERSION}"
        )));
    }

    let mut catalog = Catalog::new();
    for _ in 0..r.u32()? {
        let key = r.str()?;
        let database = read_database(&mut r)?;
        catalog.insert_database(key, database);
    }

    // Trailing bytes mean the writer and reader disagree about the layout,
    // which is exactly the silent-corruption case the version guard exists for.
    if !r.is_empty() {
        return Err(malformed("state image has trailing bytes"));
    }
    Ok(catalog)
}

fn malformed(what: impl std::fmt::Display) -> EngineError {
    EngineError::new(1105, "HY000", format!("Corrupt state image: {what}"))
}

// ------------------------------------------------------------------ catalog

fn write_database(w: &mut Writer, database: &Database) {
    w.str(&database.name);
    w.str(&database.charset);
    w.str(&database.collation);

    w.u32(database.tables.len() as u32);
    for (key, table) in &database.tables {
        w.str(key);
        write_table(w, table);
    }

    w.u32(database.views.len() as u32);
    for (key, view) in &database.views {
        w.str(key);
        write_view(w, view);
    }
}

fn read_database(r: &mut Reader<'_>) -> EngineResult<Database> {
    // Read into bindings first, in write order. A struct literal would read
    // its fields in the order they are *written there*, so reordering them to
    // match the declaration — the sort of tidy-up nobody thinks twice about —
    // would silently change the format with no compile error and no test
    // failure until two builds exchanged an image.
    let name = r.str()?;
    let charset = r.str()?;
    let collation = r.str()?;

    let mut database = Database {
        name,
        charset,
        collation,
        tables: BTreeMap::new(),
        views: BTreeMap::new(),
    };

    for _ in 0..r.u32()? {
        let key = r.str()?;
        database.tables.insert(key, read_table(r)?);
    }
    for _ in 0..r.u32()? {
        let key = r.str()?;
        database.views.insert(key, read_view(r)?);
    }
    Ok(database)
}

fn write_table(w: &mut Writer, table: &Table) {
    w.str(&table.name);

    w.u32(table.columns.len() as u32);
    for column in &table.columns {
        write_column(w, column);
    }

    w.u32(table.rows.len() as u32);
    for row in &table.rows {
        w.u32(row.len() as u32);
        for value in row {
            write_value(w, value);
        }
    }

    w.u32(table.indexes.len() as u32);
    for index in &table.indexes {
        w.str(&index.name);
        w.strs(&index.columns);
        w.bool(index.unique);
        w.bool(index.primary);
    }

    w.u32(table.foreign_keys.len() as u32);
    for fk in &table.foreign_keys {
        w.str(&fk.name);
        w.strs(&fk.columns);
        w.str(&fk.referenced_table);
        w.strs(&fk.referenced_columns);
        w.str(&fk.on_delete);
        w.str(&fk.on_update);
    }

    w.u32(table.checks.len() as u32);
    for check in &table.checks {
        w.str(&check.name);
        w.str(&check.expr);
    }

    w.opt_str(table.collation.as_deref());
    w.opt_str(table.comment.as_deref());
    w.i64(table.next_auto_increment);
}

fn read_table(r: &mut Reader<'_>) -> EngineResult<Table> {
    let name = r.str()?;

    let mut columns = Vec::new();
    for _ in 0..r.u32()? {
        columns.push(read_column(r)?);
    }

    let row_count = r.u32()?;
    let mut rows = Vec::with_capacity(row_count.min(4096) as usize);
    for _ in 0..row_count {
        let width = r.u32()?;
        let mut row = Vec::with_capacity(width.min(4096) as usize);
        for _ in 0..width {
            row.push(read_value(r)?);
        }
        rows.push(row);
    }

    let mut indexes = Vec::new();
    for _ in 0..r.u32()? {
        let name = r.str()?;
        let columns = r.strs()?;
        let unique = r.bool()?;
        let primary = r.bool()?;
        indexes.push(Index {
            name,
            columns,
            unique,
            primary,
        });
    }

    let mut foreign_keys = Vec::new();
    for _ in 0..r.u32()? {
        let name = r.str()?;
        let columns = r.strs()?;
        let referenced_table = r.str()?;
        let referenced_columns = r.strs()?;
        let on_delete = r.str()?;
        let on_update = r.str()?;
        foreign_keys.push(ForeignKey {
            name,
            columns,
            referenced_table,
            referenced_columns,
            on_delete,
            on_update,
        });
    }

    let mut checks = Vec::new();
    for _ in 0..r.u32()? {
        let name = r.str()?;
        let expr = r.str()?;
        checks.push(Check { name, expr });
    }

    let collation = r.opt_str()?;
    let comment = r.opt_str()?;
    let next_auto_increment = r.i64()?;

    Ok(Table {
        name,
        columns,
        rows,
        indexes,
        foreign_keys,
        checks,
        collation,
        comment,
        next_auto_increment,
    })
}

fn write_view(w: &mut Writer, view: &View) {
    w.str(&view.name);
    w.str(&view.definition);
    w.strs(&view.columns);
    w.u32(view.shape.len() as u32);
    for column in &view.shape {
        write_column(w, column);
    }
}

fn read_view(r: &mut Reader<'_>) -> EngineResult<View> {
    let name = r.str()?;
    let definition = r.str()?;
    let columns = r.strs()?;
    let mut shape = Vec::new();
    for _ in 0..r.u32()? {
        shape.push(read_column(r)?);
    }
    Ok(View {
        name,
        definition,
        columns,
        shape,
    })
}

fn write_column(w: &mut Writer, column: &Column) {
    w.str(&column.name);
    write_type(w, &column.ty);
    w.bool(column.nullable);
    match &column.default {
        Some(value) => {
            w.bool(true);
            write_value(w, value);
        }
        None => w.bool(false),
    }
    w.opt_str(column.default_expr.as_deref());
    w.bool(column.auto_increment);
    w.bool(column.primary_key);
    w.bool(column.unique);
    w.opt_str(column.comment.as_deref());
    w.opt_str(column.collation.as_deref());
    w.u32(column.position as u32);
    w.opt_str(column.generated.as_deref());
    w.bool(column.on_update_now);
}

fn read_column(r: &mut Reader<'_>) -> EngineResult<Column> {
    // In write order, and bound before the struct for the reason above.
    let name = r.str()?;
    let ty = read_type(r)?;
    let nullable = r.bool()?;
    let default = if r.bool()? {
        Some(read_value(r)?)
    } else {
        None
    };
    let default_expr = r.opt_str()?;
    let auto_increment = r.bool()?;
    let primary_key = r.bool()?;
    let unique = r.bool()?;
    let comment = r.opt_str()?;
    let collation = r.opt_str()?;
    let position = r.u32()? as usize;
    let generated = r.opt_str()?;
    let on_update_now = r.bool()?;

    Ok(Column {
        name,
        ty,
        nullable,
        default,
        default_expr,
        auto_increment,
        primary_key,
        unique,
        comment,
        collation,
        position,
        generated,
        on_update_now,
    })
}

fn write_type(w: &mut Writer, ty: &SqlType) {
    w.u8(type_tag(ty.kind));
    w.str(&ty.declared);
    match ty.length {
        Some(n) => {
            w.bool(true);
            w.u64(n);
        }
        None => w.bool(false),
    }
    match ty.scale {
        Some(n) => {
            w.bool(true);
            w.u64(n);
        }
        None => w.bool(false),
    }
    w.bool(ty.unsigned);
    w.strs(&ty.variants);
    // Appended, as the format's rule requires: v9. Nothing in the DDL declares
    // a binary-collated column, but `CREATE TABLE ... AS SELECT` off
    // `information_schema` copies one into a user table, and a column that
    // loses the flag across a save and a reload describes itself differently
    // than it did before.
    w.bool(ty.binary_collation);
}

fn read_type(r: &mut Reader<'_>) -> EngineResult<SqlType> {
    let kind = type_kind(r.u8()?)?;
    let declared = r.str()?;
    let length = if r.bool()? { Some(r.u64()?) } else { None };
    let scale = if r.bool()? { Some(r.u64()?) } else { None };
    let unsigned = r.bool()?;
    let variants = r.strs()?;
    let binary_collation = r.bool()?;

    Ok(SqlType {
        kind,
        declared,
        length,
        scale,
        unsigned,
        variants,
        binary_collation,
    })
}

/// Type tags are written out rather than derived from the enum's order, so
/// adding a variant cannot renumber the ones already in a baked image.
fn type_tag(kind: TypeKind) -> u8 {
    match kind {
        TypeKind::Bool => 0,
        TypeKind::Int => 1,
        TypeKind::Float => 2,
        TypeKind::Decimal => 3,
        TypeKind::Text => 4,
        TypeKind::Blob => 5,
        TypeKind::Date => 6,
        TypeKind::DateTime => 7,
        TypeKind::Time => 8,
        TypeKind::Year => 9,
        TypeKind::Json => 10,
        TypeKind::Enum => 11,
        TypeKind::Set => 12,
    }
}

fn type_kind(tag: u8) -> EngineResult<TypeKind> {
    Ok(match tag {
        0 => TypeKind::Bool,
        1 => TypeKind::Int,
        2 => TypeKind::Float,
        3 => TypeKind::Decimal,
        4 => TypeKind::Text,
        5 => TypeKind::Blob,
        6 => TypeKind::Date,
        7 => TypeKind::DateTime,
        8 => TypeKind::Time,
        9 => TypeKind::Year,
        10 => TypeKind::Json,
        11 => TypeKind::Enum,
        12 => TypeKind::Set,
        other => return Err(malformed(format!("unknown type tag {other}"))),
    })
}

fn write_value(w: &mut Writer, value: &Value) {
    match value {
        Value::Null => w.u8(0),
        Value::Int(n) => {
            w.u8(1);
            w.i64(*n);
        }
        // Written as bits rather than text: a decimal rendering would not round
        // trip, and a row that changes on reload is a wrong answer.
        Value::Float(f) => {
            w.u8(2);
            w.u64(f.to_bits());
        }
        Value::Text(s) => {
            w.u8(3);
            w.str(s);
        }
        Value::Bytes(b) => {
            w.u8(4);
            w.u32(b.len() as u32);
            w.bytes(b);
        }
        // Appended, never inserted: tag 5 is new and 0 to 4 mean what they
        // always did. A reader of this build still reads every tag an older
        // one wrote.
        Value::Json(s) => {
            w.u8(5);
            w.str(s);
        }
        // Tag 6, appended for the same reason 5 was: a decimal's text is what
        // it *is*, scale and all, and calling it a string on the way out would
        // reload it as one — comparing by characters where MySQL compares by
        // value.
        Value::Decimal(s) => {
            w.u8(6);
            w.str(s);
        }
        // Tag 7. Four bytes, because that is the whole point of the tag: a
        // reload as a double would print every digit of the widening.
        Value::Float32(f) => {
            w.u8(7);
            w.u32(f.to_bits());
        }
        // Tag 8, appended like every tag before it. A temporal reloaded as a
        // string would read 2024 where the column reads 20240105, which is the
        // whole defect the tag closes.
        Value::Temporal(s) => {
            w.u8(8);
            w.str(s);
        }
        // Tag 9, appended like every tag before it. Both fields, in this
        // order, because neither can be derived from the other here: the
        // ordinal is the column's declaration order and a value is read back
        // without one. An ENUM reloaded as a string reads 0 in arithmetic,
        // which is the whole defect the tag closes.
        Value::Enum { member, ordinal } => {
            w.u8(9);
            w.str(member);
            w.u32(*ordinal);
        }
        // Tag 10, appended like every tag before it, and written as the `u64`
        // it is rather than through an `i64` — the top half of the range is
        // the reason the tag exists, so a signed field here would lose exactly
        // the values it was added to carry.
        Value::Unsigned(n) => {
            w.u8(10);
            w.u64(*n);
        }
        // No tag of its own, and so no format version: a boolean cannot be
        // stored. Every value entering a row goes through
        // `SqlType::coerce`, and every arm of it builds a fresh value of the
        // column's own kind — a `BOOLEAN` column is a `TINYINT(1)` and holds
        // the integer, which is MySQL's rule too: `JSON_ARRAY(b)` over such a
        // column holding 1 is `[1]` there, not `[true]`. So this arm is what
        // the row would already have held, written under the tag it would
        // already have had.
        Value::Bool(b) => {
            w.u8(1);
            w.i64(i64::from(*b));
        }
    }
}

fn read_value(r: &mut Reader<'_>) -> EngineResult<Value> {
    Ok(match r.u8()? {
        0 => Value::Null,
        1 => Value::Int(r.i64()?),
        2 => Value::Float(f64::from_bits(r.u64()?)),
        3 => Value::Text(r.str()?),
        4 => {
            let len = r.u32()?;
            Value::Bytes(r.bytes(len as usize)?.to_vec())
        }
        5 => {
            // Checked on the way in, so a `Value::Json` always holds something
            // that parses. A truncated or hand-edited image would otherwise
            // load rows that evaluate to JSON null inside every constructor
            // and comparison, which is a wrong answer where the rest of the
            // engine raises 3140 or 3141.
            let text = r.str()?;
            let Ok(parsed) = crate::json::parse_document(&text) else {
                return Err(malformed(format!(
                    "value is tagged JSON but is not: {text:?}"
                )));
            };
            // Rendered rather than taken as written, so an image made before
            // documents were stored in MySQL's own form loads as though it had
            // not been. Otherwise the same data reads back differently
            // depending on whether it arrived through an INSERT or a reload,
            // which is the kind of difference nothing would think to look for.
            Value::Json(crate::json::render_text(&parsed))
        }
        6 => Value::Decimal(r.str()?),
        7 => Value::Float32(f32::from_bits(r.u32()?)),
        8 => Value::Temporal(r.str()?),
        // Bound in write order rather than built as a struct literal, so a
        // reordering here is a compile error and not a silently swapped pair.
        9 => {
            let member = r.str()?;
            let ordinal = r.u32()?;
            Value::Enum { member, ordinal }
        }
        10 => Value::Unsigned(r.u64()?),
        other => return Err(malformed(format!("unknown value tag {other}"))),
    })
}

// --------------------------------------------------------------- primitives

struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self {
            // Images are megabytes, so the first few growth steps are wasted
            // copies of the whole buffer.
            out: Vec::with_capacity(64 * 1024),
        }
    }

    fn finish(self) -> Vec<u8> {
        self.out
    }

    fn u8(&mut self, v: u8) {
        self.out.push(v);
    }

    fn bool(&mut self, v: bool) {
        self.out.push(u8::from(v));
    }

    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    fn i64(&mut self, v: i64) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    fn bytes(&mut self, v: &[u8]) {
        self.out.extend_from_slice(v);
    }

    fn str(&mut self, v: &str) {
        self.u32(v.len() as u32);
        self.out.extend_from_slice(v.as_bytes());
    }

    fn opt_str(&mut self, v: Option<&str>) {
        match v {
            Some(s) => {
                self.bool(true);
                self.str(s);
            }
            None => self.bool(false),
        }
    }

    fn strs(&mut self, v: &[String]) {
        self.u32(v.len() as u32);
        for s in v {
            self.str(s);
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn bytes(&mut self, len: usize) -> EngineResult<&'a [u8]> {
        let end = self
            .at
            .checked_add(len)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| malformed("image ends mid-value"))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn u8(&mut self) -> EngineResult<u8> {
        Ok(self.bytes(1)?[0])
    }

    fn bool(&mut self) -> EngineResult<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(malformed(format!("{other} where a flag was expected"))),
        }
    }

    fn u32(&mut self) -> EngineResult<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> EngineResult<u64> {
        let b = self.bytes(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn i64(&mut self) -> EngineResult<i64> {
        Ok(self.u64()? as i64)
    }

    fn str(&mut self) -> EngineResult<String> {
        let len = self.u32()? as usize;
        let bytes = self.bytes(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| malformed("a string is not valid UTF-8"))
    }

    fn opt_str(&mut self) -> EngineResult<Option<String>> {
        if self.bool()? {
            Ok(Some(self.str()?))
        } else {
            Ok(None)
        }
    }

    fn strs(&mut self) -> EngineResult<Vec<String>> {
        let len = self.u32()?;
        let mut out = Vec::with_capacity(len.min(4096) as usize);
        for _ in 0..len {
            out.push(self.str()?);
        }
        Ok(out)
    }
}
