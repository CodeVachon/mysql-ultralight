//! What executing a statement produces.

use mysql_ultralight_protocol::column::charset;
use mysql_ultralight_protocol::{ColumnDef, ColumnType};

use crate::types::SqlType;
use crate::value::Value;

/// The widest `Item::max_length` a materialised integer expression still fits
/// an `int` at.
///
/// MySQL writes the test as `item->max_length > MY_INT32_NUM_DECIMAL_DIGITS -
/// 2`, and `MY_INT32_NUM_DECIMAL_DIGITS` is 11 — the eleven characters
/// `-2147483648` takes. The two it subtracts are the sign and one digit, so
/// the widest expression that still gets an `int` field prints nine
/// characters. Measured through a view on 8.3: `99999999` is `int` and
/// `999999999` is `bigint`, one digit before `INT_MAX` would have needed it.
const INT_MAX_LENGTH: u32 = 9;

/// One column of a result set.
#[derive(Debug, Clone)]
pub struct OutColumn {
    pub name: String,
    pub table: String,
    pub schema: String,
    pub ty: ColumnType,
    pub nullable: bool,
    /// The type this column was declared with, where it has one.
    ///
    /// The wire type is a widening: `INT`, `SMALLINT` and `BIGINT` all leave
    /// as `LONGLONG`, and every string type as `VAR_STRING`. That is fine for
    /// a result — the client only needs to know how to decode it — but a view
    /// stores its shape from these columns, and rebuilding an `SqlType` from
    /// the wire type reports `bigint` for an `INT` column and `text` for a
    /// `VARCHAR(40)`.
    ///
    /// `None` for a computed expression, which genuinely has no declared type:
    /// inventing one would be worse than widening it.
    pub declared: Option<SqlType>,
    /// The stored column this one is a direct projection of, where it is one.
    ///
    /// The *source* column's own name, which is not `name`: an alias renames
    /// the result column and leaves the underlying one alone. MySQL builds a
    /// `CREATE TABLE ... AS SELECT` column from the source `Field` and takes
    /// only the name from the alias, so `SELECT id AS name FROM src` declares
    /// `name int NOT NULL DEFAULT '0'` — `id`'s attributes under `name`'s
    /// name. Looking the default up by `name` instead found `src.name` and
    /// copied a `VARCHAR` default into an `INT` column, which then stored the
    /// string `'x'` in it.
    ///
    /// `None` for anything computed, which has no stored column behind it.
    pub origin: Option<String>,
    /// The stored table that column lives in, which is not `table`.
    ///
    /// `table` is what the query *called* it — the alias where the FROM clause
    /// gave one — and a default has to be read out of the real table. Looking
    /// it up by the alias was not merely a miss: `SELECT Other2.name FROM Src
    /// Other2`, where a table named `Other2` also exists, found that table's
    /// `name` column and copied its default into a column built from `Src`.
    /// 8.3 declares `'x'` there, out of `Src`.
    ///
    /// `None` where the column did not come from a stored table, and where the
    /// path that produced it does not know which one — the grouped projection
    /// has the columns but not the relation behind them, so a `CREATE TABLE
    /// ... AS SELECT name FROM src GROUP BY name` takes the implicit default
    /// where 8.3 copies the source's. A missing default rather than a wrong
    /// one, which is the side to be on.
    pub origin_table: Option<String>,
    /// Whether a computed column's values are bytes rather than characters.
    ///
    /// A separate field for the same reason `unsigned` below is one: a
    /// declared column answers this from its `SqlType`, and an expression has
    /// none to answer from. `UNHEX` and `FROM_BASE64` are the two that need
    /// it — 8.3 describes both with the binary character set and the BINARY
    /// flag, and a client reads exactly that to hand back a Buffer rather than
    /// a String.
    ///
    /// It is not MySQL's rule, which is the expression's collation and
    /// propagates through every string function: `CONCAT(b, '')` over a
    /// `BLOB` is binary there and text here. That is #0128, and this field is
    /// the narrow part of it a function can state about its own answer.
    pub binary_values: bool,
    /// Whether the values are unsigned, for a column with no declared type to
    /// say so.
    ///
    /// A separate field rather than a fabricated `declared`, because a view
    /// stores its shape from these columns and would then record a column type
    /// nothing declared. `ROW_NUMBER()` counts from one and MySQL marks it
    /// UNSIGNED; so does `CAST(x AS UNSIGNED)`, which says so in the statement.
    pub unsigned: bool,
    /// How wide a computed column's widest value can print, where this engine
    /// models it.
    ///
    /// MySQL calls this `Item::max_length`. For a number it counts the
    /// characters — `SELECT 1` is 2, `SELECT 127` is 4, a `DECIMAL(10,2)` sum
    /// is its precision plus a point and a sign — and for a string it counts
    /// the bytes of the result's character set. It decides two things: the
    /// width on the wire, and the *type code* a materialised copy of an
    /// integer column gets — see [`Self::materialised_sql_type`]. It is
    /// carried here rather than recomputed because a view stores its shape
    /// from these columns and has no expression left to ask.
    ///
    /// `None` for a declared column, which answers from its declaration, and
    /// for every expression whose width this engine does not model. None means
    /// "the type's default width", which is the answer this engine gave for
    /// every expression before this field existed — 20 for a number and 4096
    /// for a string.
    pub max_length: Option<u32>,
    /// How many decimal places a computed column shows, where this engine
    /// models it.
    ///
    /// MySQL's `Item::decimals`, which is a fact about the *expression* and
    /// not about any column behind it: `a + b` over a `DECIMAL(10,2)` and a
    /// `DECIMAL(7,3)` shows three places on 8.3, and `SELECT 'x'` shows
    /// `NOT_FIXED_DEC`. A declared column answers this from its own type in
    /// [`SqlType::wire_decimals`] instead, and the two are deliberately
    /// separate: a field with no scale reports 0 where an item with none
    /// reports 31.
    ///
    /// `None` for a declared column and for the expressions this engine does
    /// not model, which then report 0 — what every column here reported before
    /// this field existed.
    pub decimals: Option<u8>,
}

impl OutColumn {
    pub fn new(name: impl Into<String>, ty: ColumnType) -> Self {
        Self {
            name: name.into(),
            table: String::new(),
            schema: String::new(),
            ty,
            nullable: true,
            unsigned: false,
            binary_values: false,
            declared: None,
            origin: None,
            origin_table: None,
            max_length: None,
            decimals: None,
        }
    }

    pub fn from_sql_type(name: impl Into<String>, ty: &SqlType, nullable: bool) -> Self {
        Self {
            name: name.into(),
            table: String::new(),
            schema: String::new(),
            ty: ty.column_type(),
            nullable,
            unsigned: ty.unsigned,
            // A declared column answers this from its own type, in the
            // `binary_collation` and charset arms of `to_wire` below.
            binary_values: false,
            declared: Some(ty.clone()),
            origin: None,
            origin_table: None,
            // A declared column carries its own width and places in
            // `declared`, and materialising one copies the field rather than
            // deriving a new one: `SELECT ti FROM (SELECT ti FROM t) d` is a
            // TINY on 8.3 where `SELECT ti + 0` through the same derived table
            // is not.
            max_length: None,
            decimals: None,
        }
    }

    /// Records the stored column this one is a direct projection of, and the
    /// table that column lives in. See `origin` and `origin_table`.
    ///
    /// The table is an `Option` because one caller genuinely does not have it:
    /// both are attributes of the same projection, so they are set together
    /// rather than in two calls that could disagree.
    pub fn projected_from(mut self, column: &str, base_table: Option<&str>) -> Self {
        self.origin = Some(column.to_owned());
        self.origin_table = base_table.map(str::to_owned);
        self
    }

    /// The type a relation built from this column should carry.
    ///
    /// The declared type where there is one, and the widening the wire type
    /// implies where there is not — which is what every caller used to do
    /// unconditionally.
    ///
    /// The sign travels either way. A derived table, a CTE and a view are all
    /// built from this, and the widened fallback is signed, so wrapping a
    /// computed column in a subquery used to lose a flag the bare expression
    /// had.
    ///
    /// So does binariness, for the same reason and with the same consequence.
    /// A relation built from this is re-described through
    /// [`OutColumn::from_sql_type`], which reads *only* the type — so a
    /// widening that came back textual made `SELECT x FROM (SELECT UNHEX(x)
    /// AS x) d` a utf8mb4 column carrying bytes that are not UTF-8, which is
    /// the exact mangling `UNHEX` answering bytes exists to stop. MySQL agrees
    /// that the widening is a *type*: a view over `UNHEX('4142')` is declared
    /// `varbinary(8)` on 8.3, and the derived table, the CTE and the UNION all
    /// come back `cs=63 flags=[BINARY]`.
    ///
    /// The width is 512 rather than 8 for the same reason the textual
    /// widening is `varchar(512)` — the byte width a string function's answer
    /// has is not a *declaration* this engine can spell, since the codes it
    /// reaches for have none between `varchar` and the four blob sizes — and
    /// the two are kept the same shape deliberately. The exact widths are in
    /// `max_length`, which the wire reads and this does not.
    ///
    /// A decimal is the one widening that is not a guess. Its precision and
    /// scale are the arithmetic in `exec`'s `exact_shape`, reassembled by
    /// [`Self::widened_decimal`] just below, and MySQL declares exactly
    /// them: `CREATE TABLE t AS SELECT a + b` over a `DECIMAL(10,2)` and a
    /// `DECIMAL(7,3)` is a `decimal(12,3)` on 8.3, where this engine used to
    /// widen every computed decimal to a bare `decimal` — precision 10, scale
    /// 0 — and report a view over a money sum with no places at all.
    pub fn sql_type(&self) -> SqlType {
        self.declared.clone().unwrap_or_else(|| {
            let mut widened = if self.binary_values {
                SqlType::widened_binary()
            } else if let Some(decimal) = self.widened_decimal() {
                decimal
            } else if let Some(text) = self.widened_text() {
                text
            } else {
                SqlType::from_column_type(self.ty)
            };
            // Only where the flag describes a *number*. UNSIGNED is a wire
            // fact about the item and a hex literal carries it while being a
            // string: `SELECT X'41'` is VAR_STRING UNSIGNED on 8.3. Carried
            // into the materialised type it would declare a
            // `varbinary(512) unsigned`, which is not a type, and hand it to
            // whatever reads `COLUMN_TYPE` — and MySQL does not keep the flag
            // through a materialisation anyway: `SELECT * FROM (SELECT X'41'
            // AS h) z` is a VAR_STRING with no UNSIGNED there, where the same
            // derived table over an `INT UNSIGNED` column keeps it.
            if self.unsigned && is_numeric(self.ty) {
                widened.unsigned = true;
                // The spelling too, not only the flag: `declared` *is*
                // `COLUMN_TYPE`, so a view built from this reported a `bigint`
                // whose precision was the unsigned one, and Prisma reads that
                // column to decide whether a field is unsigned.
                widened.declared.push_str(" unsigned");
            }
            widened
        })
    }

    /// The `DECIMAL(M,D)` a computed decimal column widens to, where its shape
    /// is known.
    ///
    /// The precision is recovered from the width and the places rather than
    /// carried as a third field, which is MySQL's own arithmetic — its
    /// `my_decimal_length_to_precision` is exactly this, and its inverse is
    /// what `SqlType::wire_length` already does for a declared column. Keeping
    /// one number instead of two means the width on the wire and the width in
    /// a view cannot disagree.
    fn widened_decimal(&self) -> Option<SqlType> {
        if !matches!(self.ty, ColumnType::NewDecimal | ColumnType::Decimal) {
            return None;
        }
        let length = u64::from(self.max_length?);
        let scale = u64::from(self.decimals?);
        let point = u64::from(scale > 0);
        let sign = u64::from(!self.unsigned);
        // Not `saturating_sub`: a width that cannot hold its own point and
        // sign is not a decimal shape at all, and inventing a zero precision
        // for it would declare `decimal(0,3)`. Widening as before says less.
        let precision = length.checked_sub(point + sign)?;
        Some(SqlType {
            kind: crate::types::TypeKind::Decimal,
            declared: format!("decimal({precision},{scale})"),
            length: Some(precision),
            scale: Some(scale),
            unsigned: self.unsigned,
            variants: Vec::new(),
            binary_collation: false,
            collation: crate::collation::Collation::Ai0900Ci,
        })
    }

    /// The TEXT type a computed column under a blob code widens to.
    ///
    /// A string function reaches for a blob code once its answer outgrows a
    /// VARCHAR — `UPPER(tx)` over a `TEXT` is a MEDIUM_BLOB on 8.3 — and the
    /// answer is still *characters*. Without this arm the widening runs
    /// through `SqlType::from_column_type`, which reads a blob code as a
    /// `blob` and therefore as bytes, and a derived table over `UPPER(tx)`
    /// would hand the caller a Buffer it cannot convert. That is the
    /// `GROUP_CONCAT` defect the differential harness exists to catch, and
    /// widening the type codes is what would have re-introduced it.
    ///
    /// `longtext` whichever blob code it was, which is measured rather than
    /// matched size for size: `CREATE TABLE t AS SELECT UPPER(tx)` over a
    /// `TEXT` column declares `ut longtext` on 8.3, and the item it was built
    /// from is a MEDIUM_BLOB. MySQL sizes the *item* from the argument and the
    /// *field* it materialises into to the largest of the family.
    ///
    /// `None` where the values really are bytes, which `binary_values` states
    /// and the caller checks first, and for a computed JSON document, which
    /// MySQL does describe with the binary character set.
    fn widened_text(&self) -> Option<SqlType> {
        if !matches!(
            self.ty,
            ColumnType::TinyBlob | ColumnType::Blob | ColumnType::MediumBlob | ColumnType::LongBlob
        ) {
            return None;
        }
        Some(SqlType {
            kind: crate::types::TypeKind::Text,
            declared: "longtext".to_owned(),
            length: None,
            scale: None,
            unsigned: false,
            variants: Vec::new(),
            binary_collation: false,
            collation: crate::collation::Collation::Ai0900Ci,
        })
    }

    /// Whether materialising this column narrows the type code it goes out
    /// under.
    ///
    /// Asked before [`Self::materialised_sql_type`] by the callers for whom
    /// answering "is this block materialised at all?" costs a parse: no column
    /// narrows without a width, and almost none have one.
    /// The type code is checked as well as the width, and it holds today for
    /// free: everything `printed_width` models is typed LONGLONG by
    /// `computed_column_type`, so this can only ever be looking at a `bigint`.
    /// It is here for the day that stops being true — `max_length` now carries
    /// a decimal's width and a string's too, and neither narrows. This engine
    /// types `CASE WHEN 1 THEN ti END` over a `TINYINT` as a TINY, and a width
    /// for that shape — which 8.3 declares `int`, so one is worth having —
    /// would otherwise relabel a `tinyint(1)` as an `int` while leaving the
    /// wire code a TINY, and the binary protocol would send one byte for a
    /// column announced as four.
    pub(crate) fn narrows(&self) -> bool {
        self.max_length.is_some_and(|w| w <= INT_MAX_LENGTH)
            && self.declared.is_none()
            && self.ty == ColumnType::LongLong
    }

    /// The type this column takes in a temporary table MySQL builds from it.
    ///
    /// [`Self::sql_type`] plus the one narrowing a materialisation performs:
    /// an integer *expression* lands in a field sized for the values it can
    /// print, so a `bigint` that can never need more than nine characters
    /// becomes an `int`. MySQL's own condition is `item->max_length >
    /// MY_INT32_NUM_DECIMAL_DIGITS - 2` in `create_tmp_field_from_item`, which
    /// is 9 — measured rather than read: through a view, `99999999` (nine
    /// characters with its sign) is `int` on 8.3 and `999999999` (ten) is
    /// `bigint`, and the same step is what makes `CREATE VIEW LM AS SELECT 1
    /// MOD 3` an `int` there where this engine said `bigint`. That was #0110.
    ///
    /// Only an expression narrows, and the *spelling* is what says so. A
    /// column reference carries a width like anything else — `printed_width`
    /// reads one so that `ti + 1` can be sized from it — but it also carries
    /// its declaration, and MySQL copies that field rather than deriving a new
    /// one: `SELECT ti FROM (SELECT ti FROM t) d` is a TINY on 8.3 and not a
    /// LONG, and so are `+ti` and `(ti)`, which MySQL'''s parser hands straight
    /// back. `bigint` is the only thing an integer *expression* is widened to
    /// here, so asking for that spelling is what separates the two.
    ///
    /// Which callers use this and which use `sql_type` is the other half of
    /// the rule, and it is not a detail: MySQL *merges* a derived table or a
    /// view that has a FROM clause into the enclosing query instead of
    /// materialising it, and a merged column is still the expression, LONGLONG
    /// and all. See `materialised_by_mysql`.
    pub fn materialised_sql_type(&self) -> SqlType {
        let mut ty = self.sql_type();
        if self.narrows() {
            ty.declared = if ty.unsigned {
                "int unsigned".to_owned()
            } else {
                "int".to_owned()
            };
        }
        ty
    }

    pub fn in_table(mut self, schema: &str, table: &str) -> Self {
        self.schema = schema.to_owned();
        self.table = table.to_owned();
        self
    }

    /// The wire-protocol column definition.
    pub fn to_wire(&self) -> ColumnDef {
        let mut def = ColumnDef::new(self.name.clone(), self.ty)
            .with_schema(self.schema.clone())
            .with_table(self.table.clone());
        // Only a declared type knows whether a `MYSQL_TYPE_BLOB` is a `TEXT`
        // or a `BLOB`; the code is the same and the charset is the difference.
        // A computed column keeps whatever its type code implies, which is
        // what it had before there was a declared type to ask.
        if let Some(declared) = &self.declared {
            def = def.as_text(declared.is_textual());
            // The width MySQL sends is the one the column was declared with.
            // #0126.
            def = def.with_length(declared.wire_length());
            // And the places, which is a *declaration's* question and not an
            // expression's — see `SqlType::wire_decimals`. #0111.
            def = def.with_decimals(declared.wire_decimals());
        } else {
            // A computed column has no declaration to read, so it answers from
            // the `Item::max_length` and `Item::decimals` arithmetic the walk
            // in `exec` models. Where it models neither, the type's default
            // stands — 20 for a number, 4096 for a string, 0 places — which is
            // what every expression here reported before #0111.
            if let Some(length) = self.max_length {
                def = def.with_length(length);
            }
            if let Some(decimals) = self.decimals {
                def = def.with_decimals(decimals);
            }
        }
        // Without this the binary protocol reads the bytes back signed, so an
        // `INT UNSIGNED` holding three billion arrives as a negative number. It
        // only became reachable once the column stopped being described as a
        // `LONGLONG`, where the value still fit.
        if self.unsigned {
            def = def.unsigned();
        }
        // The BLOB flag is read off the field, so a stored TEXT, BLOB or JSON
        // column carries it and an expression that produces one does not:
        // `GROUP_CONCAT` is a `MYSQL_TYPE_LONG_BLOB` with no flag on 8.3.
        let blob = matches!(
            self.ty,
            ColumnType::Blob
                | ColumnType::TinyBlob
                | ColumnType::MediumBlob
                | ColumnType::LongBlob
                | ColumnType::Json
        );
        // An expression under a blob type code is a string function's answer,
        // and MySQL sends it as text. `GROUP_CONCAT(v)` over a `VARCHAR` is
        // `cs=45 flags=[]` on 8.3, and so are `CONCAT(t,'x')`, `LOWER(t)` and
        // `HEX(b)`; `JSON_UNQUOTE` is `cs=45 flags=[BINARY]`, binary by its
        // `utf8mb4_bin` collation and still sent in characters. Every
        // expression this engine gives a blob code is one of those —
        // `GROUP_CONCAT`, `JSON_UNQUOTE` and `->>` — where the *default* for a
        // blob code in `ColumnDef::new` is the binary character set, which is
        // what a client reads to hand back bytes instead of a String.
        //
        // A computed document goes the same way and keeps the flag: 8.3 sends
        // `JSON_OBJECT('a',1)` as `cs=45 flags=[BINARY]` — characters on the
        // wire, binary by collation, which is the shape `binary_collation`
        // describes for a stored column. A *stored* JSON column is different
        // and is left alone: that one really is `cs=63`.
        //
        // What this cannot follow is MySQL's real rule, which is the operand's
        // collation: `GROUP_CONCAT(b)` over a `BLOB` is binary on 8.3 and text
        // here, and so is `UPPER(NULL)`. This engine has no collation to
        // propagate, and text is the side that keeps a string a string.
        let computed_document = self.declared.is_none() && self.ty == ColumnType::Json;
        if self.declared.is_none() && blob {
            def = def.as_text(true);
        }
        // The one expression that says its own answer is bytes. Set after the
        // blob correction above rather than before it, so a function that ever
        // answers bytes under a blob code is not turned back into text by a
        // rule about `GROUP_CONCAT`.
        if self.binary_values {
            def = def.as_text(false);
        }
        // Binary is a claim about the character set first: a column whose
        // values are characters is not binary, whatever else it is — with one
        // exception, below. Among the columns whose values are bytes, whether
        // MySQL calls one binary depends on which kind of column it is. A
        // *declared* column is a field, and a field is binary for a temporal,
        // a BLOB or a JSON document but never for a number, which is why an
        // `INT` column carries no BINARY flag on 8.3. A *computed* column is
        // an item, and an item is binary whenever its collation is — which
        // includes every number, so `SELECT 1` is `len=2 flags=[BINARY]`
        // there while the `INT` column it was added to is not.
        //
        // The exception is a `_bin` collation, the one way a column of
        // characters is binary: MySQL sends the text in the connection's
        // character set and sets the flag anyway. Every `information_schema`
        // column a client introspects with arrives that way.
        let binary = self.declared.as_ref().is_some_and(|d| d.binary_collation)
            || computed_document
            || (def.charset == charset::BINARY
                && (self.declared.is_none() || !is_numeric(self.ty)));
        // Read off the declaration and nowhere else, which is what makes a
        // materialised ENUM lose it: a set operation clears `declared` because
        // MySQL does not carry an ENUM through one, and a derived table, a CTE
        // and a view all keep it because MySQL keeps it there too — measured
        // on 8.3, `SELECT e FROM (SELECT e FROM t) d` is `flags=[ENUM]` and
        // `SELECT e FROM t UNION ALL SELECT e FROM t` is a VAR_STRING with no
        // flag at all.
        def.member_list(self.declared.as_ref().and_then(SqlType::member_list))
            .blob(self.declared.is_some() && blob)
            .binary(binary)
            // A stored YEAR only. `YEAR(x)` is UNSIGNED and not zero-filled.
            .zerofill(self.declared.is_some() && self.ty == ColumnType::Year)
            .nullable(self.nullable)
    }
}

/// Rows plus their metadata.
#[derive(Debug, Clone, Default)]
pub struct ResultSet {
    pub columns: Vec<OutColumn>,
    pub rows: Vec<Vec<Value>>,
}

impl ResultSet {
    pub fn new(columns: Vec<OutColumn>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    /// Makes every value agree with the type its column is described with.
    ///
    /// The type is decided in `computed_column_type` and the value is tagged
    /// by the evaluator, in different code, for the same expression. Where
    /// they disagree the two protocols answer differently for the same row:
    /// the binary one encodes by the declared code, so a column described
    /// FLOAT holding a widened double writes four bytes of a number the text
    /// protocol printed to seventeen digits. That is a whole class of bug
    /// rather than a list of functions to remember, so it is closed here
    /// instead — every result passes through this on its way out.
    ///
    /// The approximate types, because they are the ones where the same number
    /// has two spellings — and two exact pairings under a code that is not
    /// exact: a fraction under an integer code, which is the one combination
    /// the two protocols cannot both send at all, and a decimal under a
    /// DOUBLE. Nothing else is touched: a wrongly *tagged* value is a bug to
    /// find, not one to paper over.
    pub fn reconcile(&mut self) {
        for (index, column) in self.columns.iter().enumerate() {
            // A `FLOAT(M,D)` or `DOUBLE(M,D)` shows exactly D places, always,
            // the way a DECIMAL does: 0.1 in a `FLOAT(10,4)` reads back
            // `0.1000` and not `0.1`.
            //
            // Rendered here rather than tagged on the value because the scale
            // belongs to the *column*: MySQL keeps it on the field and carries
            // it forward as an expression's `decimals`, which this engine does
            // not model — see the note on `fixed_scale`.
            let scale = fixed_scale(column);

            for row in &mut self.rows {
                let Some(value) = row.get_mut(index) else {
                    continue;
                };
                if let (Some(scale), Value::Float(_) | Value::Float32(_)) = (scale, &*value) {
                    // From the *shortest* digits that read back as this
                    // double, not from its binary expansion, which is what
                    // `format!("{:.*}")` writes. The two part company as soon
                    // as the scale runs past what a double distinguishes:
                    // 0.1 in a `DOUBLE(20,17)` is `0.10000000000000000` on 8.3
                    // and the expansion is `0.10000000000000001`, and 1.5 in a
                    // `DOUBLE(255,30)` is thirty zeros there against
                    // `1.500000000000000222044604925031`. MySQL's `my_fcvt`
                    // goes through dtoa, which stops at the shortest string
                    // that round-trips and pads the rest — so this pads too.
                    //
                    // Rounds as well as pads, and has to: `ALTER TABLE` can
                    // give a column a scale its existing rows never passed
                    // `coerce` for, and 8.3 renders those stale rows rounded.
                    //
                    // The rounding is a *rendering* and stops here, which is
                    // why the digits go on as the value and the scale as the
                    // display rather than the other way round. MySQL's own
                    // (M,D) change is metadata-only — measured on 8.3, a row
                    // written before `ALTER TABLE t MODIFY f FLOAT(10,4)`
                    // still reads back `f32:1.2345679` through a prepared
                    // statement while the text protocol prints `1.2346`, and
                    // widening the scale again brings the original digits back
                    // — so the binary protocol has to be able to reach the
                    // number the row holds. `encode_binary` takes its width
                    // from the *type code*, which is still FLOAT or DOUBLE,
                    // and used to read the number back out of the *rounded*
                    // digits: a stale row was sent as four bytes of a value
                    // that was never in the column. #0131.
                    //
                    // Tagged a decimal rather than left as text because it is
                    // a number.
                    let shortest = crate::decimal::plain(value.as_number());
                    *value = Value::decimal_at(shortest, scale);
                    continue;
                }
                match (column.ty, &value) {
                    (ColumnType::Float, Value::Float(v)) => *value = Value::Float32(*v as f32),
                    (ColumnType::Double, Value::Float32(v)) => *value = Value::Float(f64::from(*v)),
                    // An exact value under an approximate code. Reachable
                    // wherever the operand's *type* is not a temporal but its
                    // value is one: `DATE_ADD(v, INTERVAL 1 DAY)` over a
                    // VARCHAR is typed STRING by MySQL and computes as a
                    // double — `SUM` of it is DOUBLE 20260103060708.5 — while
                    // the engine packs the temporal and totals it exactly, so
                    // the text protocol wrote every declared digit and the
                    // binary one sent a rounded f64 of them. Same split the
                    // integer pairing below closes, in the other direction.
                    (ColumnType::Double, Value::Decimal(text, _)) => {
                        if let Ok(n) = text.parse::<f64>() {
                            *value = Value::Float(n);
                        }
                    }
                    // A decimal column never carries an exponent, whatever the
                    // float behind it renders as. See `decimal::plain`.
                    (ColumnType::NewDecimal | ColumnType::Decimal, Value::Float(v)) => {
                        *value = Value::decimal(crate::decimal::plain(*v));
                    }
                    // A fraction under an integer code. The text protocol
                    // writes the digits and the binary one parses them as an
                    // `i64`, fails, and writes zero — so the same row reads
                    // 60708.567 through one connection and 0 through the
                    // other. Reachable wherever an operand's declared
                    // precision is out of the metadata's reach, and this is
                    // the last place the value and the code are both in hand.
                    //
                    // Rounded rather than truncated, which is what MySQL
                    // renders an exact value as under a LONGLONG.
                    (
                        ColumnType::Tiny
                        | ColumnType::Short
                        | ColumnType::Int24
                        | ColumnType::Long
                        | ColumnType::LongLong
                        | ColumnType::Year,
                        Value::Decimal(text, _),
                    ) => {
                        if let Some(n) =
                            crate::decimal::units(text, 0).and_then(|u| i64::try_from(u).ok())
                        {
                            *value = Value::Int(n);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// A single-column, single-row result, the shape most `SELECT @@var` and
    /// scalar function queries take.
    pub fn scalar(name: &str, ty: ColumnType, value: Value) -> Self {
        Self {
            columns: vec![OutColumn::new(name, ty)],
            rows: vec![vec![value]],
        }
    }
}

/// Whether a type code is one of MySQL's numbers.
///
/// The split that decides the BINARY flag on a stored column, and it is by the
/// code rather than by the charset: a number and a date both report the binary
/// character set, and only one of them is flagged binary.
///
/// It is also what UNSIGNED can survive a materialisation on — see
/// [`OutColumn::sql_type`] and the set operation in `exec`.
pub(crate) fn is_numeric(ty: ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Tiny
            | ColumnType::Short
            | ColumnType::Int24
            | ColumnType::Long
            | ColumnType::LongLong
            | ColumnType::Float
            | ColumnType::Double
            | ColumnType::Decimal
            | ColumnType::NewDecimal
            | ColumnType::Year
            | ColumnType::Bit
    )
}

/// The number of decimal places a column shows every value at, where it has
/// one.
///
/// Only a declared `FLOAT(M,D)` or `DOUBLE(M,D)` does. `REAL(M,D)` is the same
/// type to MySQL — it reports `double(10,4)` for it — but `sqlparser` has no
/// precision on its `REAL`, so the engine answers 1064 for the spelling rather
/// than getting as far as this. A clear error, and its own gap.
///
/// MySQL keeps that D on the field as its `decimals` and propagates it through
/// an expression — `f + 0` and `SUM(f)` over a `FLOAT(10,4)` both still show
/// four places on 8.3, `AVG(f)` shows eight, and a division adds four — so the
/// rule is really about expressions and not about columns. This engine has no
/// per-expression `decimals`, so anything computed falls back to the plain
/// float rendering. That is what MySQL itself does for everything outside that
/// list — `ROUND(f, 2)`, `CAST(f AS FLOAT)` and `JSON_ARRAY(f)` all drop the
/// places there — and a divergence for the handful inside it.
fn fixed_scale(column: &OutColumn) -> Option<usize> {
    let declared = column.declared.as_ref()?;
    if declared.kind != crate::types::TypeKind::Float {
        return None;
    }
    declared.scale.map(|scale| scale as usize)
}

/// The outcome of one statement.
#[derive(Debug, Clone)]
pub enum Outcome {
    Rows(ResultSet),
    /// A statement that changed data or schema rather than returning rows.
    Affected {
        rows: u64,
        last_insert_id: u64,
    },
}

impl Outcome {
    pub fn ok() -> Self {
        Outcome::Affected {
            rows: 0,
            last_insert_id: 0,
        }
    }

    pub fn affected(rows: u64) -> Self {
        Outcome::Affected {
            rows,
            last_insert_id: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `reconcile` is a backstop: the type rules already agree with the value
    /// for every expression there is a case for, so nothing reaching it is a
    /// path a query takes *today*. That is the point of it — it holds for the
    /// function nobody has added yet — and it is why it is measured here
    /// directly rather than through a statement that happens to trip it.
    #[test]
    fn a_value_is_made_to_agree_with_the_type_it_is_described_with() {
        let mismatched = |ty, value| {
            let mut result = ResultSet {
                columns: vec![OutColumn::new("v", ty)],
                rows: vec![vec![value]],
            };
            result.reconcile();
            result.rows.remove(0).remove(0)
        };

        // A double under a FLOAT: the binary protocol writes four bytes, so
        // the value has to be the one four bytes hold.
        assert_eq!(
            mismatched(ColumnType::Float, Value::Float(1.234_567_890_123)).as_text(),
            "1.23457"
        );
        // And the other way: six digits in text beside sixteen in binary was
        // the same disagreement read from the other side.
        assert_eq!(
            mismatched(ColumnType::Double, Value::Float32(1.234_567_9)).as_text(),
            "1.2345678806304932"
        );
        // A decimal never carries an exponent, whatever the float renders as.
        assert_eq!(
            mismatched(ColumnType::NewDecimal, Value::Float(4e16)).as_text(),
            "40000000000000000"
        );

        // A fraction under an integer code, which is the one pairing the two
        // protocols cannot both send: the text one writes `60708.567` and the
        // binary one parses it as an `i64`, fails, and writes 0.
        //
        // No *statement* reaches this today — a temporal is written into its
        // column's declared precision, and the metadata reads that same
        // precision, so the two agree by construction all the way through a
        // derived table, a union and a view. It is asserted here, at its own
        // level, rather than through SQL that would have to be contrived: the
        // guard exists because this failure is invisible to any test that
        // reads the text, not because a known query produces it.
        assert_eq!(
            mismatched(ColumnType::LongLong, Value::decimal("60708.567")),
            Value::Int(60709)
        );
        assert_eq!(
            mismatched(ColumnType::Long, Value::decimal("-2.5")),
            Value::Int(-3)
        );
        // A decimal under a *decimal* code is left alone, which is the whole
        // point of the tag.
        assert_eq!(
            mismatched(ColumnType::NewDecimal, Value::decimal("60708.567")),
            Value::decimal("60708.567")
        );

        // Nothing else is touched: a wrongly tagged value elsewhere is a bug
        // to find, not one to paper over.
        assert_eq!(
            mismatched(ColumnType::LongLong, Value::text("7".to_owned())),
            Value::text("7".to_owned())
        );
    }
}
