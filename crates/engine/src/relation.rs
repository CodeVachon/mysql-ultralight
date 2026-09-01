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
    /// The database the wire metadata reports for this column.
    pub schema: String,
    /// The database a `db.table.column` qualifier must name to reach this
    /// column, empty where the FROM item is in no database at all.
    ///
    /// Not the same fact as `schema`, which is why it is not the same field.
    /// `schema` describes the *column* — 8.3 reports `probe` for a column of
    /// `(SELECT b FROM probe.J2) d`, because the metadata follows the field
    /// behind the derived table. Resolution reads the *table reference*, and
    /// that derived table is in no database: `SELECT nodb.d.b FROM (SELECT b
    /// FROM probe.J2) d` answers on 8.3 while `SELECT probe.d.*` from the same
    /// FROM item is 1051. One field could not be both.
    pub qualifier_database: String,
    pub table: String,
    pub name: String,
    pub ty: SqlType,
    pub nullable: bool,
}

impl RelColumn {
    /// This column in the shape the name matcher and the evaluator read.
    ///
    /// Everything that resolves a name goes through here, so the database is
    /// carried rather than dropped on the way — which is what it used to be.
    pub fn reference(&self) -> ColumnRef {
        ColumnRef::in_database(&self.qualifier_database, &self.table, &self.name)
    }
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
    /// Builds a relation from one stored table, under the name a qualified
    /// reference will use, and optionally without the rows.
    ///
    /// `shown` is the alias where one was given. Taking it here rather than
    /// renaming a cloned table means describing a statement never copies the
    /// rows it is not going to read — which is the whole reason preparing a
    /// query no longer costs as much as running it.
    pub fn from_table_named(schema: &str, table: &Table, shown: &str, with_rows: bool) -> Self {
        let rows = if with_rows {
            table.rows.clone()
        } else {
            Vec::new()
        };
        Self {
            columns: table
                .columns
                .iter()
                .map(|c| RelColumn {
                    schema: schema.to_owned(),
                    // A table keeps its database under an alias: `SELECT
                    // probe.x.b FROM J2 x` answers on 8.3, and `SELECT
                    // probe.x.z FROM other.T x` is 1054. The alias renames the
                    // table part and leaves the database alone.
                    qualifier_database: schema.to_owned(),
                    table: shown.to_owned(),
                    name: c.name.clone(),
                    ty: c.ty.clone(),
                    nullable: c.nullable,
                })
                .collect(),
            origins: (0..rows.len()).map(|i| vec![Some(i)]).collect(),
            rows,
            sources: vec![shown.to_owned()],
            base_tables: vec![table.name.clone()],
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
        self.columns.iter().map(RelColumn::reference).collect()
    }

    pub(crate) fn index_of(&self, written: &crate::name::ColumnParts) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.reference().matches(written))
    }

    /// Whether a `db.table.*` names a FROM item this relation carries.
    pub(crate) fn has_source(&self, database: Option<&str>, table: &str) -> bool {
        self.columns
            .iter()
            .any(|c| c.reference().in_source(database, table))
    }

    /// Appends another relation's columns to this one's, for a join.
    ///
    /// Outer-joined columns become nullable regardless of their declaration:
    /// an unmatched row fills them with NULL, and reporting them as NOT NULL
    /// would be a metadata lie a client may act on.
    /// The joined schema, with the side an outer join can pad marked nullable.
    ///
    /// Which side that is depends on the operator, and the two are not
    /// interchangeable: a `LEFT JOIN` invents NULLs for the *right* relation
    /// and a `RIGHT JOIN` for the *left*. Relaxing the right one for both made
    /// the engine flag a column `NOT NULL` and then send NULL in it.
    pub fn joined_columns(
        &self,
        other: &Relation,
        relax_self: bool,
        relax_other: bool,
    ) -> Vec<RelColumn> {
        fn relaxed(columns: &[RelColumn], relax: bool) -> impl Iterator<Item = RelColumn> + '_ {
            columns.iter().map(move |column| {
                let mut column = column.clone();
                column.nullable |= relax;
                column
            })
        }
        relaxed(&self.columns, relax_self)
            .chain(relaxed(&other.columns, relax_other))
            .collect()
    }
}
