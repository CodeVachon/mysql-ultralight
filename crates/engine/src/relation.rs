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
    /// The names rows are addressed by in join order — an alias where one was
    /// given, which is what a qualified column reference uses.
    pub sources: Vec<String>,
    /// The catalog tables behind those names, in the same order. A self-join is
    /// only expressible with aliases, so writing back needs both.
    pub base_tables: Vec<String>,
    /// For each row, where it came from in each source table.
    ///
    /// A multi-table UPDATE has to write back to the row a join produced, and
    /// a materialised copy cannot be traced without this. `None` marks the
    /// padding an outer join adds for an unmatched side.
    pub origins: Vec<Vec<Option<usize>>>,
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
            sources: vec![table.name.clone()],
            base_tables: vec![table.name.clone()],
            origins: (0..table.rows.len()).map(|i| vec![Some(i)]).collect(),
        }
    }

    /// The ordinal of a source table, for writing a joined row back.
    pub fn source_index(&self, table: &str) -> Option<usize> {
        self.sources
            .iter()
            .position(|s| s.eq_ignore_ascii_case(table))
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
