//! What executing a statement produces.

use mysql_ultralight_protocol::{ColumnDef, ColumnType};

use crate::types::SqlType;
use crate::value::Value;

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
    /// Whether the values are unsigned, for a column with no declared type to
    /// say so.
    ///
    /// A separate field rather than a fabricated `declared`, because a view
    /// stores its shape from these columns and would then record a column type
    /// nothing declared. `ROW_NUMBER()` counts from one and MySQL marks it
    /// UNSIGNED; so does `CAST(x AS UNSIGNED)`, which says so in the statement.
    pub unsigned: bool,
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
            declared: None,
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
            declared: Some(ty.clone()),
        }
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
    pub fn sql_type(&self) -> SqlType {
        self.declared.clone().unwrap_or_else(|| {
            let mut widened = SqlType::from_column_type(self.ty);
            if self.unsigned {
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
        }
        // Without this the binary protocol reads the bytes back signed, so an
        // `INT UNSIGNED` holding three billion arrives as a negative number. It
        // only became reachable once the column stopped being described as a
        // `LONGLONG`, where the value still fit.
        if self.unsigned {
            def = def.unsigned();
        }
        def.nullable(self.nullable)
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
    /// Only the approximate types, because they are the ones where the same
    /// number has two spellings. Nothing else is touched: a wrongly *tagged*
    /// value is a bug to find, not one to paper over.
    pub fn reconcile(&mut self) {
        for (index, column) in self.columns.iter().enumerate() {
            for row in &mut self.rows {
                let Some(value) = row.get_mut(index) else {
                    continue;
                };
                match (column.ty, &value) {
                    (ColumnType::Float, Value::Float(v)) => *value = Value::Float32(*v as f32),
                    (ColumnType::Double, Value::Float32(v)) => *value = Value::Float(f64::from(*v)),
                    // A decimal column never carries an exponent, whatever the
                    // float behind it renders as. See `decimal::plain`.
                    (ColumnType::NewDecimal | ColumnType::Decimal, Value::Float(v)) => {
                        *value = Value::Decimal(crate::decimal::plain(*v));
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

        // Nothing else is touched: a wrongly tagged value elsewhere is a bug
        // to find, not one to paper over.
        assert_eq!(
            mismatched(ColumnType::LongLong, Value::Text("7".to_owned())),
            Value::Text("7".to_owned())
        );
    }
}
