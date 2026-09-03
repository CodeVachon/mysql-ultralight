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

/// Where one join source's rows live, and how a name can reach it.
///
/// Two facts rather than one, because a multi-table DELETE asks both and an
/// alias separates them: which database a *target* may write in front of the
/// source's name, and which database the rows would actually come out of.
/// Measured on 8.3, over a `T` in `other` while the session is on `probe`:
/// `DELETE other.a FROM other.T a JOIN K ON a.a = K.a` is 1109 — the database
/// spelled right and the alias will not take it — while `DELETE a FROM other.T
/// a JOIN K ON a.a = K.a` deletes from `other`.`T`. So an alias hides the
/// database from the name and not from the write, and one field could not be
/// both.
///
/// Neither of them is `RelColumn::qualifier_database`, which is the same
/// question asked about a *column* and has the opposite answer again: `SELECT
/// probe.x.b FROM T x` answers, and so does `SELECT probe.x.*`.
#[derive(Debug, Clone)]
pub struct SourceDatabase {
    /// The database this source's rows live in, which is the one a write to
    /// them has to reach.
    pub holds: String,
    /// Whether `holds` may be written in front of this source's name.
    pub addressable: bool,
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
    /// Where each source's rows live and how a name reaches it, in the same
    /// order. See [`SourceDatabase`].
    pub source_databases: Vec<SourceDatabase>,
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
    ///
    /// `addressable` is whether a DELETE target may write `schema` in front of
    /// `shown`, which an alias makes false. See [`SourceDatabase`].
    pub fn from_table_named(
        schema: &str,
        addressable: bool,
        table: &Table,
        shown: &str,
        with_rows: bool,
    ) -> Self {
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
            source_databases: vec![SourceDatabase {
                holds: schema.to_owned(),
                addressable,
            }],
        }
    }

    /// The ordinal of a source table, for writing a joined row back.
    pub fn source_index(&self, table: &str) -> Option<usize> {
        self.sources
            .iter()
            .position(|s| s.eq_ignore_ascii_case(table))
    }

    /// The source a multi-table DELETE target names, if the FROM clause has
    /// one.
    ///
    /// `database` is what the statement wrote in front of the target and
    /// `current` the session's database, which an absent one falls back to:
    /// `DELETE A FROM other.A JOIN B ON other.A.id = B.id` is 1109 on 8.3
    /// rather than a delete from `other`, because the bare `A` means
    /// `current.A` and the FROM clause holds no such item.
    ///
    /// Kept apart from [`ColumnRef::in_source`], which answers the same shape
    /// of question for a `db.table.*` and answers it differently — an alias
    /// keeps its database there and loses it here. One comparer serving both
    /// would have to be told which rule to apply, which is the argument that
    /// gave the database part two readers and no comparison at all.
    pub(crate) fn delete_target(
        &self,
        database: Option<&str>,
        current: &str,
        table: &str,
    ) -> Option<usize> {
        // Zipped rather than indexed, so the two vectors being parallel is
        // what the code does rather than something it assumes.
        self.sources
            .iter()
            .zip(&self.source_databases)
            .position(|(source, in_database)| {
                source.eq_ignore_ascii_case(table)
                    && match database {
                        // A database written in front of the name reaches only
                        // an item that answers to one, and only its own.
                        Some(written) => {
                            in_database.addressable
                                && in_database.holds.eq_ignore_ascii_case(written)
                        }
                        // No database means the session's, against an item
                        // that could have carried one — and means nothing at
                        // all against an alias, which answers to itself.
                        None => {
                            !in_database.addressable
                                || in_database.holds.eq_ignore_ascii_case(current)
                        }
                    }
            })
    }

    /// The database one source's rows live in, for the write that has to
    /// reach them.
    ///
    /// `None` only where the index names no source, which the callers'
    /// indices — all of them from `source_index` or `delete_target` — cannot
    /// produce today. It is an `Option` rather than an index so that a future
    /// FROM item pushing a source without a [`SourceDatabase`] is a refusal
    /// rather than a panic in the middle of a user's DELETE.
    pub(crate) fn source_holds(&self, index: usize) -> Option<&str> {
        self.source_databases
            .get(index)
            .map(|source| source.holds.as_str())
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
