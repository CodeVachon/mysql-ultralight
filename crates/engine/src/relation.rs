//! A materialised set of rows with column metadata, produced by a FROM clause.
//!
//! Joins are nested-loop over fully materialised inputs. That is the wrong
//! algorithm for a production database and the right one here: test tables are
//! small, and it keeps the executor small enough to be obviously correct.

use crate::catalog::Table;
use crate::expr::ColumnRef;
use crate::types::SqlType;
use crate::value::Value;

/// One column of a relation, carrying enough metadata to describe it on the
/// wire if it is projected.
#[derive(Debug, Clone)]
pub struct RelColumn {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub ty: SqlType,
    pub nullable: bool,
}

/// Rows plus their column metadata.
#[derive(Debug, Clone, Default)]
pub struct Relation {
    pub columns: Vec<RelColumn>,
    pub rows: Vec<Vec<Value>>,
}

impl Relation {
    /// Builds a relation from one stored table.
    pub fn from_table(schema: &str, table: &Table) -> Self {
        Self {
            columns: table
                .columns
                .iter()
                .map(|c| RelColumn {
                    schema: schema.to_owned(),
                    table: table.name.clone(),
                    name: c.name.clone(),
                    ty: c.ty.clone(),
                    nullable: c.nullable,
                })
                .collect(),
            rows: table.rows.clone(),
        }
    }

    /// Column references in the shape the expression evaluator wants.
    pub fn refs(&self) -> Vec<ColumnRef> {
        self.columns
            .iter()
            .map(|c| ColumnRef::new(&c.table, &c.name))
            .collect()
    }

    pub fn index_of(&self, table: Option<&str>, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| {
            c.name.eq_ignore_ascii_case(name)
                && table.is_none_or(|t| c.table.eq_ignore_ascii_case(t))
        })
    }

    /// Appends another relation's columns to this one's, for a join.
    ///
    /// Outer-joined columns become nullable regardless of their declaration:
    /// an unmatched row fills them with NULL, and reporting them as NOT NULL
    /// would be a metadata lie a client may act on.
    pub fn joined_columns(&self, other: &Relation, force_nullable: bool) -> Vec<RelColumn> {
        let mut columns = self.columns.clone();
        for column in &other.columns {
            let mut column = column.clone();
            if force_nullable {
                column.nullable = true;
            }
            columns.push(column);
        }
        columns
    }
}
