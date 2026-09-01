//! Foreign key enforcement.
//!
//! 735 foreign keys are declared across the target's migrations. Recording them
//! for introspection without enforcing them means a test that expects a
//! constraint violation gets a successful write instead — a wrong answer, not a
//! missing feature.

use std::collections::HashSet;

use crate::catalog::{Database, ForeignKey, Table};
use crate::error::{EngineError, EngineResult};
use crate::name;
use crate::value::Value;

/// The parent keys a child table's foreign key can point at, captured before
/// the child is mutated.
///
/// Snapshotting the keys rather than holding a borrow keeps validation a pure
/// function of the candidate row, so the child table can be borrowed mutably
/// at the same time.
#[derive(Debug, Clone)]
pub struct ParentKeys {
    pub constraint: ForeignKey,
    /// Positions of the referencing columns in the child row.
    pub child_positions: Vec<usize>,
    /// Positions of the referenced columns, when the parent is this same table.
    /// A self-referencing multi-row INSERT must see rows added earlier in the
    /// same statement, which MySQL checks row by row.
    self_positions: Option<Vec<usize>>,
    keys: HashSet<Vec<String>>,
    parent_missing: bool,
}

impl ParentKeys {
    /// Whether a candidate child row satisfies the constraint.
    pub fn accepts(&self, row: &[Value]) -> bool {
        // A parent table that does not exist cannot be checked against. MySQL
        // rejects the DDL in that case; here the constraint is simply inert
        // rather than failing every write.
        if self.parent_missing {
            return true;
        }

        let key: Vec<String> = self
            .child_positions
            .iter()
            .map(|p| row.get(*p).cloned().unwrap_or(Value::Null))
            .map(|v| v.collation_key())
            .collect();

        // A NULL anywhere in the referencing columns exempts the row, as in
        // MySQL: the reference is simply absent.
        if self
            .child_positions
            .iter()
            .any(|p| row.get(*p).is_some_and(Value::is_null))
        {
            return true;
        }

        self.keys.contains(&key)
    }

    /// Records a row just inserted into the child table, for the case where
    /// the constraint points at that same table.
    pub fn note_inserted(&mut self, row: &[Value]) {
        let Some(positions) = &self.self_positions else {
            return;
        };
        self.keys.insert(
            positions
                .iter()
                .map(|p| row.get(*p).cloned().unwrap_or(Value::Null).collation_key())
                .collect(),
        );
    }

    pub fn violation(&self, child_table: &str) -> EngineError {
        EngineError::new(
            1452,
            "23000",
            format!(
                "Cannot add or update a child row: a foreign key constraint fails \
                 ({}, CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}))",
                name::quote(child_table),
                name::quote(&self.constraint.name),
                quoted_list(&self.constraint.columns),
                name::quote(&self.constraint.referenced_table),
                quoted_list(&self.constraint.referenced_columns),
            ),
        )
    }
}

/// Captures the parent keys for every foreign key on `table`.
pub fn snapshot(database: &Database, table: &Table) -> Vec<ParentKeys> {
    table
        .foreign_keys
        .iter()
        .filter_map(|constraint| {
            let child_positions: Vec<usize> = constraint
                .columns
                .iter()
                .filter_map(|c| table.column_index(c))
                .collect();
            if child_positions.len() != constraint.columns.len() {
                return None;
            }

            let parent = database.table(&constraint.referenced_table);
            let keys = match parent {
                Some(parent) => {
                    let positions: Vec<usize> = constraint
                        .referenced_columns
                        .iter()
                        .filter_map(|c| parent.column_index(c))
                        .collect();
                    if positions.len() != constraint.referenced_columns.len() {
                        return None;
                    }
                    parent
                        .rows
                        .iter()
                        .map(|row| {
                            positions
                                .iter()
                                .map(|p| row.get(*p).cloned().unwrap_or(Value::Null))
                                .map(|v| v.collation_key())
                                .collect()
                        })
                        .collect()
                }
                None => HashSet::new(),
            };

            let self_positions = constraint
                .referenced_table
                .eq_ignore_ascii_case(&table.name)
                .then(|| {
                    constraint
                        .referenced_columns
                        .iter()
                        .filter_map(|c| table.column_index(c))
                        .collect::<Vec<usize>>()
                })
                .filter(|p| p.len() == constraint.referenced_columns.len());

            Some(ParentKeys {
                constraint: constraint.clone(),
                child_positions,
                self_positions,
                keys,
                parent_missing: parent.is_none(),
            })
        })
        .collect()
}

/// Checks a candidate child row against every captured constraint.
pub fn check(parents: &[ParentKeys], row: &[Value], child_table: &str) -> EngineResult<()> {
    for parent in parents {
        if !parent.accepts(row) {
            return Err(parent.violation(child_table));
        }
    }
    Ok(())
}

/// A child table that references `parent`, with the column positions involved.
#[derive(Debug, Clone)]
pub struct Dependent {
    pub table: String,
    pub constraint: ForeignKey,
    /// Positions of the referencing columns in the child row.
    pub child_positions: Vec<usize>,
    /// Positions of the referenced columns in the parent row.
    pub parent_positions: Vec<usize>,
}

/// Finds every table that references `parent`.
///
/// Needed on DELETE and on an UPDATE of a parent key, where the declared
/// referential action decides what happens to the children.
pub fn dependents(database: &Database, parent: &Table) -> Vec<Dependent> {
    let mut out = Vec::new();
    for child in database.tables.values() {
        for constraint in &child.foreign_keys {
            if !constraint
                .referenced_table
                .eq_ignore_ascii_case(&parent.name)
            {
                continue;
            }
            let child_positions: Vec<usize> = constraint
                .columns
                .iter()
                .filter_map(|c| child.column_index(c))
                .collect();
            let parent_positions: Vec<usize> = constraint
                .referenced_columns
                .iter()
                .filter_map(|c| parent.column_index(c))
                .collect();
            if child_positions.len() != constraint.columns.len()
                || parent_positions.len() != constraint.referenced_columns.len()
            {
                continue;
            }
            out.push(Dependent {
                table: child.name.clone(),
                constraint: constraint.clone(),
                child_positions,
                parent_positions,
            });
        }
    }
    out
}

/// What to do with a child row whose parent is going away or changing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Cascade,
    SetNull,
    Restrict,
}

impl Action {
    /// Parses a referential action as sqlparser renders it.
    ///
    /// MySQL's default, and its reading of NO ACTION, is RESTRICT.
    pub fn parse(text: &str) -> Self {
        let upper = text.to_ascii_uppercase();
        if upper.contains("CASCADE") {
            Action::Cascade
        } else if upper.contains("SET NULL") {
            Action::SetNull
        } else {
            Action::Restrict
        }
    }
}

/// The error raised when a RESTRICT action blocks a parent write.
pub fn restrict_violation(
    child_table: &str,
    parent_table: &str,
    constraint: &ForeignKey,
) -> EngineError {
    // MySQL names the child table first, not the constraint. A client parsing
    // the table out of a 1451 would otherwise read the constraint name.
    EngineError::new(
        1451,
        "23000",
        format!(
            "Cannot delete or update a parent row: a foreign key constraint fails \
             ({}, CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}))",
            name::quote(child_table),
            name::quote(&constraint.name),
            quoted_list(&constraint.columns),
            name::quote(parent_table),
            quoted_list(&constraint.referenced_columns),
        ),
    )
}

/// A column list as one of these messages writes it: each name quoted as SQL,
/// separated by commas.
///
/// Every name here goes through `name::quote` rather than being joined with a
/// literal `` `, ` ``, which is what these messages did — correct only while
/// the names reaching them still carried their own doubling. 8.3 writes
/// ``FOREIGN KEY (`pi``d`)`` for a column named ``pi`d``.
fn quoted_list(columns: &[String]) -> String {
    columns
        .iter()
        .map(|c| name::quote(c))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referential_actions_parse_with_restrict_as_the_default() {
        assert_eq!(Action::parse("CASCADE"), Action::Cascade);
        assert_eq!(Action::parse("ON DELETE CASCADE"), Action::Cascade);
        assert_eq!(Action::parse("SET NULL"), Action::SetNull);
        assert_eq!(Action::parse("RESTRICT"), Action::Restrict);
        // NO ACTION is RESTRICT in MySQL, and so is an unrecognised value.
        assert_eq!(Action::parse("NO ACTION"), Action::Restrict);
        assert_eq!(Action::parse(""), Action::Restrict);
    }
}
