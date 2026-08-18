//! `information_schema`, generated from the catalog on read.
//!
//! Prisma's migration engine runs nine introspection queries against these
//! tables before it emits any DDL, joining them to one another. Serving them as
//! ordinary tables means normal query execution applies — joins, filters and
//! ordering all work without a special case.

use crate::catalog::{Catalog, Column, Table, INFORMATION_SCHEMA};
use crate::error::{unsupported, EngineResult};
use crate::session::Session;
use crate::types::{SqlType, TypeKind};
use crate::value::Value;

/// Builds the named `information_schema` table.
pub fn table(catalog: &Catalog, session: &Session, name: &str) -> EngineResult<Table> {
    let lower = name.to_ascii_lowercase();
    Ok(match lower.as_str() {
        "schemata" => schemata(catalog),
        "tables" => tables(catalog),
        "columns" => columns(catalog),
        "statistics" => statistics(catalog),
        "table_constraints" => table_constraints(catalog),
        "key_column_usage" => key_column_usage(catalog),
        "referential_constraints" => referential_constraints(catalog),
        // Views, routines, events, triggers and check constraints are all
        // empty rather than absent. Prisma reads each one and treats a missing
        // table as an error, but an empty result as "none defined".
        "views" => empty(
            "VIEWS",
            &[
                "TABLE_CATALOG",
                "TABLE_SCHEMA",
                "TABLE_NAME",
                "VIEW_DEFINITION",
                "CHECK_OPTION",
                "IS_UPDATABLE",
                "DEFINER",
                "SECURITY_TYPE",
                "CHARACTER_SET_CLIENT",
                "COLLATION_CONNECTION",
            ],
        ),
        "routines" => empty(
            "ROUTINES",
            &[
                "SPECIFIC_NAME",
                "ROUTINE_CATALOG",
                "ROUTINE_SCHEMA",
                "ROUTINE_NAME",
                "ROUTINE_TYPE",
                "DATA_TYPE",
                "ROUTINE_BODY",
                "ROUTINE_DEFINITION",
                "IS_DETERMINISTIC",
                "SQL_DATA_ACCESS",
                "SECURITY_TYPE",
                "DEFINER",
            ],
        ),
        "check_constraints" => empty(
            "CHECK_CONSTRAINTS",
            &[
                "CONSTRAINT_CATALOG",
                "CONSTRAINT_SCHEMA",
                "CONSTRAINT_NAME",
                "CHECK_CLAUSE",
            ],
        ),
        "triggers" => empty(
            "TRIGGERS",
            &[
                "TRIGGER_CATALOG",
                "TRIGGER_SCHEMA",
                "TRIGGER_NAME",
                "EVENT_MANIPULATION",
                "EVENT_OBJECT_TABLE",
                "ACTION_STATEMENT",
                "ACTION_TIMING",
            ],
        ),
        "events" => empty(
            "EVENTS",
            &[
                "EVENT_CATALOG",
                "EVENT_SCHEMA",
                "EVENT_NAME",
                "DEFINER",
                "EVENT_DEFINITION",
            ],
        ),
        "engines" => engines(),
        "collations" => collations(),
        "character_sets" => character_sets(),
        "processlist" => processlist(session),
        other => return Err(unsupported(format!("{INFORMATION_SCHEMA}.{other}"))),
    })
}

/// A table of all-text columns with the given names.
fn build(name: &str, headers: &[&str], rows: Vec<Vec<Value>>) -> Table {
    let mut table = Table::new(name);
    table.columns = headers
        .iter()
        .enumerate()
        .map(|(i, header)| {
            let mut column = Column::new(*header, text_type());
            column.position = i + 1;
            column
        })
        .collect();
    table.rows = rows;
    table
}

fn empty(name: &str, headers: &[&str]) -> Table {
    build(name, headers, Vec::new())
}

fn text_type() -> SqlType {
    SqlType {
        kind: TypeKind::Text,
        declared: "varchar(512)".to_owned(),
        length: Some(512),
        scale: None,
        unsigned: false,
        variants: Vec::new(),
    }
}

fn text(s: impl Into<String>) -> Value {
    Value::Text(s.into())
}

fn schemata(catalog: &Catalog) -> Table {
    let mut rows = vec![vec![
        text("def"),
        text(INFORMATION_SCHEMA),
        text("utf8mb3"),
        text("utf8mb3_general_ci"),
        Value::Null,
    ]];
    for name in catalog.database_names() {
        let db = catalog.database(&name);
        rows.push(vec![
            text("def"),
            text(name),
            text(db.map(|d| d.charset.clone()).unwrap_or_default()),
            text(db.map(|d| d.collation.clone()).unwrap_or_default()),
            Value::Null,
        ]);
    }
    build(
        "SCHEMATA",
        &[
            "CATALOG_NAME",
            "SCHEMA_NAME",
            "DEFAULT_CHARACTER_SET_NAME",
            "DEFAULT_COLLATION_NAME",
            "SQL_PATH",
        ],
        rows,
    )
}

fn tables(catalog: &Catalog) -> Table {
    let mut rows = Vec::new();
    for name in catalog.database_names() {
        let Some(db) = catalog.database(&name) else {
            continue;
        };
        for table in db.tables.values() {
            rows.push(vec![
                text("def"),
                text(&name),
                text(&table.name),
                text("BASE TABLE"),
                text("InnoDB"),
                Value::Int(10),
                text("Dynamic"),
                Value::Int(table.rows.len() as i64),
                text(
                    table
                        .collation
                        .clone()
                        .unwrap_or_else(|| db.collation.clone()),
                ),
                text(table.comment.clone().unwrap_or_default()),
                // CREATE_OPTIONS is read by Prisma; empty is the normal value.
                text(""),
            ]);
        }
    }
    build(
        "TABLES",
        &[
            "TABLE_CATALOG",
            "TABLE_SCHEMA",
            "TABLE_NAME",
            "TABLE_TYPE",
            "ENGINE",
            "VERSION",
            "ROW_FORMAT",
            "TABLE_ROWS",
            "TABLE_COLLATION",
            "TABLE_COMMENT",
            "CREATE_OPTIONS",
        ],
        rows,
    )
}

fn columns(catalog: &Catalog) -> Table {
    let mut rows = Vec::new();
    for name in catalog.database_names() {
        let Some(db) = catalog.database(&name) else {
            continue;
        };
        for table in db.tables.values() {
            for column in &table.columns {
                let numeric = matches!(
                    column.ty.kind,
                    TypeKind::Int | TypeKind::Decimal | TypeKind::Float
                );
                rows.push(vec![
                    text("def"),
                    text(&name),
                    text(&table.name),
                    text(&column.name),
                    Value::Int(column.position as i64),
                    // COLUMN_DEFAULT distinguishes "no default" (NULL) from a
                    // default of the string "NULL".
                    match (&column.default, &column.default_expr) {
                        (Some(v), _) => Value::Text(v.as_text()),
                        (None, Some(e)) => Value::Text(e.clone()),
                        (None, None) => Value::Null,
                    },
                    text(if column.nullable { "YES" } else { "NO" }),
                    text(column.ty.base_name()),
                    match column.ty.kind {
                        TypeKind::Text => column
                            .ty
                            .length
                            .map(|l| Value::Int(l as i64))
                            .unwrap_or(Value::Null),
                        _ => Value::Null,
                    },
                    if numeric {
                        column
                            .ty
                            .length
                            .map(|l| Value::Int(l as i64))
                            .unwrap_or(Value::Null)
                    } else {
                        Value::Null
                    },
                    if numeric {
                        Value::Int(column.ty.scale.unwrap_or(0) as i64)
                    } else {
                        Value::Null
                    },
                    match column.ty.kind {
                        TypeKind::DateTime | TypeKind::Time => {
                            Value::Int(column.ty.length.unwrap_or(0) as i64)
                        }
                        _ => Value::Null,
                    },
                    text(&column.ty.declared),
                    text(if column.primary_key {
                        "PRI"
                    } else if column.unique {
                        "UNI"
                    } else {
                        ""
                    }),
                    text(if column.auto_increment {
                        "auto_increment"
                    } else {
                        ""
                    }),
                    text(
                        column
                            .collation
                            .clone()
                            .unwrap_or_else(|| db.collation.clone()),
                    ),
                    text(column.comment.clone().unwrap_or_default()),
                ]);
            }
        }
    }
    build(
        "COLUMNS",
        &[
            "TABLE_CATALOG",
            "TABLE_SCHEMA",
            "TABLE_NAME",
            "COLUMN_NAME",
            "ORDINAL_POSITION",
            "COLUMN_DEFAULT",
            "IS_NULLABLE",
            "DATA_TYPE",
            "CHARACTER_MAXIMUM_LENGTH",
            "NUMERIC_PRECISION",
            "NUMERIC_SCALE",
            "DATETIME_PRECISION",
            "COLUMN_TYPE",
            "COLUMN_KEY",
            "EXTRA",
            "COLLATION_NAME",
            "COLUMN_COMMENT",
        ],
        rows,
    )
}

fn statistics(catalog: &Catalog) -> Table {
    let mut rows = Vec::new();
    for name in catalog.database_names() {
        let Some(db) = catalog.database(&name) else {
            continue;
        };
        for table in db.tables.values() {
            for index in &table.indexes {
                for (position, column) in index.columns.iter().enumerate() {
                    rows.push(vec![
                        text("def"),
                        text(&name),
                        text(&table.name),
                        // NON_UNIQUE is inverted: 0 means the index is unique.
                        Value::Int(i64::from(!(index.unique || index.primary))),
                        text(&name),
                        text(&index.name),
                        Value::Int(position as i64 + 1),
                        text(column),
                        text("A"),
                        Value::Null,
                        // SUB_PART is the prefix length, absent for whole-column
                        // indexes.
                        Value::Null,
                        text("BTREE"),
                    ]);
                }
            }
        }
    }
    build(
        "STATISTICS",
        &[
            "TABLE_CATALOG",
            "TABLE_SCHEMA",
            "TABLE_NAME",
            "NON_UNIQUE",
            "INDEX_SCHEMA",
            "INDEX_NAME",
            "SEQ_IN_INDEX",
            "COLUMN_NAME",
            "COLLATION",
            "CARDINALITY",
            "SUB_PART",
            "INDEX_TYPE",
        ],
        rows,
    )
}

fn table_constraints(catalog: &Catalog) -> Table {
    let mut rows = Vec::new();
    for name in catalog.database_names() {
        let Some(db) = catalog.database(&name) else {
            continue;
        };
        for table in db.tables.values() {
            for index in table.indexes.iter().filter(|i| i.unique || i.primary) {
                rows.push(vec![
                    text("def"),
                    text(&name),
                    text(&index.name),
                    text(&name),
                    text(&table.name),
                    text(if index.primary {
                        "PRIMARY KEY"
                    } else {
                        "UNIQUE"
                    }),
                    text("YES"),
                ]);
            }
            for fk in &table.foreign_keys {
                rows.push(vec![
                    text("def"),
                    text(&name),
                    text(&fk.name),
                    text(&name),
                    text(&table.name),
                    text("FOREIGN KEY"),
                    text("YES"),
                ]);
            }
        }
    }
    build(
        "TABLE_CONSTRAINTS",
        &[
            "CONSTRAINT_CATALOG",
            "CONSTRAINT_SCHEMA",
            "CONSTRAINT_NAME",
            "TABLE_SCHEMA",
            "TABLE_NAME",
            "CONSTRAINT_TYPE",
            "ENFORCED",
        ],
        rows,
    )
}

fn key_column_usage(catalog: &Catalog) -> Table {
    let mut rows = Vec::new();
    for name in catalog.database_names() {
        let Some(db) = catalog.database(&name) else {
            continue;
        };
        for table in db.tables.values() {
            for index in table.indexes.iter().filter(|i| i.unique || i.primary) {
                for (position, column) in index.columns.iter().enumerate() {
                    rows.push(vec![
                        text("def"),
                        text(&name),
                        text(&index.name),
                        text(&name),
                        text(&table.name),
                        text(column),
                        Value::Int(position as i64 + 1),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                    ]);
                }
            }
            for fk in &table.foreign_keys {
                for (position, column) in fk.columns.iter().enumerate() {
                    rows.push(vec![
                        text("def"),
                        text(&name),
                        text(&fk.name),
                        text(&name),
                        text(&table.name),
                        text(column),
                        Value::Int(position as i64 + 1),
                        Value::Int(position as i64 + 1),
                        text(&name),
                        text(&fk.referenced_table),
                        text(
                            fk.referenced_columns
                                .get(position)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    ]);
                }
            }
        }
    }
    build(
        "KEY_COLUMN_USAGE",
        &[
            "CONSTRAINT_CATALOG",
            "CONSTRAINT_SCHEMA",
            "CONSTRAINT_NAME",
            "TABLE_SCHEMA",
            "TABLE_NAME",
            "COLUMN_NAME",
            "ORDINAL_POSITION",
            "POSITION_IN_UNIQUE_CONSTRAINT",
            "REFERENCED_TABLE_SCHEMA",
            "REFERENCED_TABLE_NAME",
            "REFERENCED_COLUMN_NAME",
        ],
        rows,
    )
}

fn referential_constraints(catalog: &Catalog) -> Table {
    let mut rows = Vec::new();
    for name in catalog.database_names() {
        let Some(db) = catalog.database(&name) else {
            continue;
        };
        for table in db.tables.values() {
            for fk in &table.foreign_keys {
                rows.push(vec![
                    text("def"),
                    text(&name),
                    text(&fk.name),
                    text("def"),
                    text(&name),
                    text("PRIMARY"),
                    text("NONE"),
                    text(&fk.on_update),
                    text(&fk.on_delete),
                    text(&table.name),
                    text(&fk.referenced_table),
                ]);
            }
        }
    }
    build(
        "REFERENTIAL_CONSTRAINTS",
        &[
            "CONSTRAINT_CATALOG",
            "CONSTRAINT_SCHEMA",
            "CONSTRAINT_NAME",
            "UNIQUE_CONSTRAINT_CATALOG",
            "UNIQUE_CONSTRAINT_SCHEMA",
            "UNIQUE_CONSTRAINT_NAME",
            "MATCH_OPTION",
            "UPDATE_RULE",
            "DELETE_RULE",
            "TABLE_NAME",
            "REFERENCED_TABLE_NAME",
        ],
        rows,
    )
}

fn engines() -> Table {
    build(
        "ENGINES",
        &[
            "ENGINE",
            "SUPPORT",
            "COMMENT",
            "TRANSACTIONS",
            "XA",
            "SAVEPOINTS",
        ],
        vec![vec![
            text("InnoDB"),
            text("DEFAULT"),
            text("mysql-ultralight in-memory storage"),
            text("YES"),
            text("NO"),
            text("YES"),
        ]],
    )
}

fn collations() -> Table {
    build(
        "COLLATIONS",
        &[
            "COLLATION_NAME",
            "CHARACTER_SET_NAME",
            "ID",
            "IS_DEFAULT",
            "IS_COMPILED",
            "SORTLEN",
        ],
        vec![
            vec![
                text("utf8mb4_0900_ai_ci"),
                text("utf8mb4"),
                Value::Int(255),
                text("Yes"),
                text("Yes"),
                Value::Int(0),
            ],
            vec![
                text("utf8mb4_general_ci"),
                text("utf8mb4"),
                Value::Int(45),
                text(""),
                text("Yes"),
                Value::Int(1),
            ],
            vec![
                text("utf8mb4_unicode_ci"),
                text("utf8mb4"),
                Value::Int(224),
                text(""),
                text("Yes"),
                Value::Int(8),
            ],
            vec![
                text("binary"),
                text("binary"),
                Value::Int(63),
                text("Yes"),
                text("Yes"),
                Value::Int(1),
            ],
        ],
    )
}

fn character_sets() -> Table {
    build(
        "CHARACTER_SETS",
        &[
            "CHARACTER_SET_NAME",
            "DEFAULT_COLLATE_NAME",
            "DESCRIPTION",
            "MAXLEN",
        ],
        vec![
            vec![
                text("utf8mb4"),
                text("utf8mb4_0900_ai_ci"),
                text("UTF-8 Unicode"),
                Value::Int(4),
            ],
            vec![
                text("binary"),
                text("binary"),
                text("Binary pseudo charset"),
                Value::Int(1),
            ],
        ],
    )
}

fn processlist(session: &Session) -> Table {
    build(
        "PROCESSLIST",
        &[
            "ID", "USER", "HOST", "DB", "COMMAND", "TIME", "STATE", "INFO",
        ],
        vec![vec![
            Value::Int(session.connection_id as i64),
            text("root"),
            text("localhost"),
            session
                .current_database
                .clone()
                .map(Value::Text)
                .unwrap_or(Value::Null),
            text("Query"),
            Value::Int(0),
            text("executing"),
            Value::Null,
        ]],
    )
}
