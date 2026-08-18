//! The catalog: databases, tables, columns, and the rows themselves.

use std::collections::BTreeMap;

use crate::error::{EngineError, EngineResult};
use crate::types::SqlType;
use crate::value::Value;

/// A column definition.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub ty: SqlType,
    pub nullable: bool,
    pub default: Option<Value>,
    /// Set when the default is an expression evaluated per row, such as
    /// `CURRENT_TIMESTAMP`, rather than a constant.
    pub default_expr: Option<String>,
    pub auto_increment: bool,
    pub primary_key: bool,
    pub unique: bool,
    pub comment: Option<String>,
    pub collation: Option<String>,
    /// Ordinal position, 1-based, as `information_schema.columns` reports it.
    pub position: usize,
}

impl Column {
    pub fn new(name: impl Into<String>, ty: SqlType) -> Self {
        Self {
            name: name.into(),
            ty,
            nullable: true,
            default: None,
            default_expr: None,
            auto_increment: false,
            primary_key: false,
            unique: false,
            comment: None,
            collation: None,
            position: 0,
        }
    }
}

/// A named index over one or more columns.
#[derive(Debug, Clone)]
pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
}

/// A foreign key constraint.
///
/// Recorded but not yet enforced. Recording it matters even so: Prisma's
/// migration engine reads these back out of `information_schema` and compares
/// them against the schema, and a missing constraint reads as a drift.
#[derive(Debug, Clone)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}

/// A table and its rows.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
    pub indexes: Vec<Index>,
    pub foreign_keys: Vec<ForeignKey>,
    pub collation: Option<String>,
    pub comment: Option<String>,
    pub next_auto_increment: i64,
}

impl Table {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            next_auto_increment: 1,
            ..Default::default()
        }
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.column_index(name).map(|i| &self.columns[i])
    }

    /// Columns forming the primary key, in declaration order.
    pub fn primary_key_columns(&self) -> Vec<&Column> {
        self.columns.iter().filter(|c| c.primary_key).collect()
    }

    /// Checks a candidate row against unique and primary key constraints.
    ///
    /// `skip` excludes a row from the check, so an UPDATE does not collide
    /// with the row it is updating.
    pub fn check_unique(&self, candidate: &[Value], skip: Option<usize>) -> EngineResult<()> {
        for index in self.indexes.iter().filter(|i| i.unique || i.primary) {
            let positions: Vec<usize> = index
                .columns
                .iter()
                .filter_map(|c| self.column_index(c))
                .collect();
            if positions.len() != index.columns.len() {
                continue;
            }

            // A NULL never conflicts with anything, including another NULL.
            if positions
                .iter()
                .any(|p| candidate.get(*p).is_some_and(Value::is_null))
            {
                continue;
            }

            for (row_index, row) in self.rows.iter().enumerate() {
                if Some(row_index) == skip {
                    continue;
                }
                let same = positions.iter().all(|p| {
                    matches!(
                        (row.get(*p), candidate.get(*p)),
                        (Some(a), Some(b)) if a.compare(b) == Some(std::cmp::Ordering::Equal)
                    )
                });
                if same {
                    let rendered: Vec<String> = positions
                        .iter()
                        .filter_map(|p| candidate.get(*p))
                        .map(Value::as_text)
                        .collect();
                    return Err(EngineError::duplicate_entry(
                        &rendered.join("-"),
                        &index.name,
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A database, also called a schema.
#[derive(Debug, Clone, Default)]
pub struct Database {
    pub name: String,
    pub tables: BTreeMap<String, Table>,
    pub charset: String,
    pub collation: String,
}

impl Database {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tables: BTreeMap::new(),
            charset: "utf8mb4".to_owned(),
            collation: "utf8mb4_0900_ai_ci".to_owned(),
        }
    }

    /// Table lookup is case-insensitive, matching `lower_case_table_names=0`
    /// on macOS where the filesystem is case-insensitive anyway.
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.get(name).or_else(|| {
            self.tables
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
    }

    pub fn table_mut(&mut self, name: &str) -> Option<&mut Table> {
        let key = self
            .tables
            .keys()
            .find(|k| k.as_str() == name || k.eq_ignore_ascii_case(name))
            .cloned()?;
        self.tables.get_mut(&key)
    }
}

/// Every database in the server.
#[derive(Debug, Default)]
pub struct Catalog {
    databases: BTreeMap<String, Database>,
}

/// The schema name reserved for catalog introspection.
pub const INFORMATION_SCHEMA: &str = "information_schema";

impl Catalog {
    pub fn new() -> Self {
        Self {
            databases: BTreeMap::new(),
        }
    }

    pub fn database_names(&self) -> Vec<String> {
        self.databases.keys().cloned().collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.resolve_name(name).is_some()
    }

    fn resolve_name(&self, name: &str) -> Option<String> {
        self.databases
            .keys()
            .find(|k| k.as_str() == name || k.eq_ignore_ascii_case(name))
            .cloned()
    }

    pub fn database(&self, name: &str) -> Option<&Database> {
        self.resolve_name(name).and_then(|k| self.databases.get(&k))
    }

    pub fn database_mut(&mut self, name: &str) -> Option<&mut Database> {
        self.resolve_name(name)
            .and_then(move |k| self.databases.get_mut(&k))
    }

    pub fn create_database(&mut self, name: &str, if_not_exists: bool) -> EngineResult<()> {
        if self.contains(name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(EngineError::new(
                1007,
                "HY000",
                format!("Can't create database '{name}'; database exists"),
            ));
        }
        self.databases.insert(name.to_owned(), Database::new(name));
        Ok(())
    }

    pub fn drop_database(&mut self, name: &str, if_exists: bool) -> EngineResult<()> {
        match self.resolve_name(name) {
            Some(key) => {
                self.databases.remove(&key);
                Ok(())
            }
            None if if_exists => Ok(()),
            None => Err(EngineError::unknown_database(name)),
        }
    }

    /// Resolves a database by explicit name, falling back to the session's
    /// current one.
    pub fn resolve<'a>(
        &'a self,
        explicit: Option<&str>,
        current: Option<&str>,
    ) -> EngineResult<&'a Database> {
        let name = explicit.or(current).ok_or_else(EngineError::no_database)?;
        self.database(name)
            .ok_or_else(|| EngineError::unknown_database(name))
    }

    pub fn resolve_mut<'a>(
        &'a mut self,
        explicit: Option<&str>,
        current: Option<&str>,
    ) -> EngineResult<&'a mut Database> {
        let name = explicit
            .or(current)
            .ok_or_else(EngineError::no_database)?
            .to_owned();
        self.database_mut(&name)
            .ok_or_else(|| EngineError::unknown_database(&name))
    }
}
