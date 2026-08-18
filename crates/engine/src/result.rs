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
}

impl OutColumn {
    pub fn new(name: impl Into<String>, ty: ColumnType) -> Self {
        Self {
            name: name.into(),
            table: String::new(),
            schema: String::new(),
            ty,
            nullable: true,
        }
    }

    pub fn from_sql_type(name: impl Into<String>, ty: &SqlType, nullable: bool) -> Self {
        Self {
            name: name.into(),
            table: String::new(),
            schema: String::new(),
            ty: ty.column_type(),
            nullable,
        }
    }

    pub fn in_table(mut self, schema: &str, table: &str) -> Self {
        self.schema = schema.to_owned();
        self.table = table.to_owned();
        self
    }

    /// The wire-protocol column definition.
    pub fn to_wire(&self) -> ColumnDef {
        let def = ColumnDef::new(self.name.clone(), self.ty)
            .with_schema(self.schema.clone())
            .with_table(self.table.clone());
        if self.nullable {
            def.nullable()
        } else {
            def
        }
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
