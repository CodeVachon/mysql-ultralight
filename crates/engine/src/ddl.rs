//! Data definition: CREATE TABLE, ALTER TABLE, CREATE INDEX.

use sqlparser::ast::{
    AlterTableOperation, ColumnDef, ColumnOption, CreateIndex, CreateTable, ObjectName,
    RenameTableNameKind, TableConstraint,
};

use crate::catalog::{Column, ForeignKey, Index, Table};
use crate::error::{unsupported, EngineError, EngineResult};
use crate::exec::Engine;
use crate::expr;
use crate::result::Outcome;
use crate::session::Session;
use crate::types::SqlType;
use crate::value::Value;

pub fn create_table(
    engine: &Engine,
    session: &mut Session,
    create: &CreateTable,
) -> EngineResult<Outcome> {
    let (db, name) = engine.split_name_public(session, &create.name)?;

    let mut table = Table::new(&name);

    for (position, def) in create.columns.iter().enumerate() {
        table.columns.push(build_column(def, position + 1)?);
    }

    for constraint in &create.constraints {
        apply_constraint(&mut table, constraint)?;
    }

    // A column-level UNIQUE needs a real index entry, not just the metadata
    // flag: uniqueness is enforced by walking the indexes, so without one the
    // column reports UNI in information_schema while admitting duplicates.
    for name in table
        .columns
        .iter()
        .filter(|c| c.unique && !c.primary_key)
        .map(|c| c.name.clone())
        .collect::<Vec<_>>()
    {
        if !table
            .indexes
            .iter()
            .any(|i| i.unique && i.columns == [name.clone()])
        {
            table.indexes.push(Index {
                name: name.clone(),
                columns: vec![name],
                unique: true,
                primary: false,
            });
        }
    }

    // A column-level PRIMARY KEY needs the same index entry a table-level one
    // produces, or `information_schema` would not report it.
    let inline_pk: Vec<String> = table
        .columns
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| c.name.clone())
        .collect();
    if !inline_pk.is_empty() && !table.indexes.iter().any(|i| i.primary) {
        table.indexes.push(Index {
            name: "PRIMARY".to_owned(),
            columns: inline_pk,
            unique: true,
            primary: true,
        });
    }

    let mut catalog = engine.catalog_mut_public()?;
    let database = catalog
        .database_mut(&db)
        .ok_or_else(|| EngineError::unknown_database(&db))?;

    if database.table(&name).is_some() {
        if create.if_not_exists {
            return Ok(Outcome::ok());
        }
        return Err(EngineError::table_exists(&name));
    }

    database.tables.insert(name, table);
    Ok(Outcome::ok())
}

fn build_column(def: &ColumnDef, position: usize) -> EngineResult<Column> {
    let mut column = Column::new(def.name.value.clone(), SqlType::from_ast(&def.data_type)?);
    column.position = position;
    // MySQL columns are nullable unless declared otherwise, except that a
    // PRIMARY KEY column is implicitly NOT NULL.
    column.nullable = true;

    for option in &def.options {
        match &option.option {
            ColumnOption::NotNull => column.nullable = false,
            ColumnOption::Null => column.nullable = true,
            ColumnOption::PrimaryKey(_) => {
                column.primary_key = true;
                // A primary key column is implicitly NOT NULL.
                column.nullable = false;
            }
            ColumnOption::Unique(_) => column.unique = true,
            ColumnOption::Default(e) => match expr::literal_of(e) {
                Some(v) => column.default = Some(v),
                // Anything not a constant is stored as text and evaluated per
                // row, so CURRENT_TIMESTAMP differs between inserts.
                None => column.default_expr = Some(e.to_string()),
            },
            ColumnOption::Comment(c) => column.comment = Some(c.clone()),
            ColumnOption::Collation(c) => column.collation = Some(c.to_string()),
            ColumnOption::DialectSpecific(tokens) => {
                let rendered = tokens
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_ascii_lowercase();
                if rendered.contains("auto_increment") {
                    column.auto_increment = true;
                }
            }
            ColumnOption::Generated { .. } => {
                return Err(unsupported(format!(
                    "generated column `{}`",
                    def.name.value
                )))
            }
            // Character sets do not change stored values here; every string is
            // held as UTF-8 regardless.
            ColumnOption::CharacterSet(_) => {}
            ColumnOption::ForeignKey { .. } => {}
            other => return Err(unsupported(format!("column option `{other}`"))),
        }
    }

    Ok(column)
}

fn apply_constraint(table: &mut Table, constraint: &TableConstraint) -> EngineResult<()> {
    match constraint {
        TableConstraint::PrimaryKey(pk) => {
            let (columns, name) = (&pk.columns, &pk.name);
            let names = index_column_names(columns);
            for column in &mut table.columns {
                if names.iter().any(|n| n.eq_ignore_ascii_case(&column.name)) {
                    column.primary_key = true;
                    column.nullable = false;
                }
            }
            table.indexes.push(Index {
                name: name
                    .as_ref()
                    .map(|n| n.value.clone())
                    .unwrap_or_else(|| "PRIMARY".to_owned()),
                columns: names,
                unique: true,
                primary: true,
            });
        }
        TableConstraint::Unique(unique) => {
            let (columns, name) = (&unique.columns, &unique.name);
            let names = index_column_names(columns);
            if names.len() == 1 {
                if let Some(c) = table
                    .columns
                    .iter_mut()
                    .find(|c| c.name.eq_ignore_ascii_case(&names[0]))
                {
                    c.unique = true;
                }
            }
            table.indexes.push(Index {
                name: name
                    .as_ref()
                    .map(|n| n.value.clone())
                    .unwrap_or_else(|| names.join("_")),
                columns: names,
                unique: true,
                primary: false,
            });
        }
        TableConstraint::Index(index) => {
            let (columns, name) = (&index.columns, &index.name);
            let names = index_column_names(columns);
            table.indexes.push(Index {
                name: name
                    .as_ref()
                    .map(|n| n.value.clone())
                    .unwrap_or_else(|| names.join("_")),
                columns: names,
                unique: false,
                primary: false,
            });
        }
        TableConstraint::ForeignKey(fk) => {
            let (name, columns, foreign_table, referred_columns, on_delete, on_update) = (
                &fk.name,
                &fk.columns,
                &fk.foreign_table,
                &fk.referred_columns,
                &fk.on_delete,
                &fk.on_update,
            );
            table.foreign_keys.push(ForeignKey {
                name: name
                    .as_ref()
                    .map(|n| n.value.clone())
                    .unwrap_or_else(|| format!("{}_fk", table.name)),
                columns: columns.iter().map(|c| c.value.clone()).collect(),
                referenced_table: object_name(foreign_table),
                referenced_columns: referred_columns.iter().map(|c| c.value.clone()).collect(),
                on_delete: on_delete
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "RESTRICT".into()),
                on_update: on_update
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "RESTRICT".into()),
            });
        }
        // CHECK is recorded as unsupported rather than accepted and ignored:
        // silently not enforcing a constraint is a wrong answer.
        TableConstraint::Check(_) => return Err(unsupported("CHECK constraints")),
        other => return Err(unsupported(format!("table constraint `{other}`"))),
    }
    Ok(())
}

fn index_column_names(columns: &[sqlparser::ast::IndexColumn]) -> Vec<String> {
    columns
        .iter()
        .map(|c| c.column.expr.to_string().trim_matches('`').to_owned())
        .collect()
}

pub fn alter_table(
    engine: &Engine,
    session: &mut Session,
    name: &ObjectName,
    operations: &[AlterTableOperation],
    if_exists: bool,
) -> EngineResult<Outcome> {
    let (db, table_name) = engine.split_name_public(session, name)?;
    let mut catalog = engine.catalog_mut_public()?;
    let database = catalog
        .database_mut(&db)
        .ok_or_else(|| EngineError::unknown_database(&db))?;

    let Some(table) = database.table_mut(&table_name) else {
        if if_exists {
            return Ok(Outcome::ok());
        }
        return Err(EngineError::no_such_table(&table_name));
    };

    for operation in operations {
        match operation {
            AlterTableOperation::AddColumn {
                column_def,
                if_not_exists,
                ..
            } => {
                if table.column_index(&column_def.name.value).is_some() {
                    if *if_not_exists {
                        continue;
                    }
                    return Err(EngineError::new(
                        1060,
                        "42S21",
                        format!("Duplicate column name '{}'", column_def.name.value),
                    ));
                }
                let column = build_column(column_def, table.columns.len() + 1)?;
                // Back-filling NULL into a NOT NULL column stores rows that
                // violate the constraint the metadata advertises, so the
                // type's implicit default is used instead.
                let filler = match (&column.default, column.nullable) {
                    (Some(v), _) => v.clone(),
                    (None, true) => Value::Null,
                    (None, false) => implicit_default(&column.ty),
                };
                table.columns.push(column);
                // Existing rows need a value in the new position.
                for row in &mut table.rows {
                    row.push(filler.clone());
                }
            }
            AlterTableOperation::DropColumn {
                column_names,
                if_exists,
                ..
            } => {
                for column_name in column_names {
                    match table.column_index(&column_name.value) {
                        Some(index) => {
                            table.columns.remove(index);
                            for row in &mut table.rows {
                                if index < row.len() {
                                    row.remove(index);
                                }
                            }
                            for (position, column) in table.columns.iter_mut().enumerate() {
                                column.position = position + 1;
                            }
                        }
                        None if *if_exists => {}
                        None => {
                            return Err(EngineError::unknown_column(
                                &column_name.value,
                                &table_name,
                            ))
                        }
                    }
                }
            }
            AlterTableOperation::RenameColumn {
                old_column_name,
                new_column_name,
            } => {
                let index = table.column_index(&old_column_name.value).ok_or_else(|| {
                    EngineError::unknown_column(&old_column_name.value, &table_name)
                })?;
                table.columns[index].name = new_column_name.value.clone();
            }
            AlterTableOperation::RenameTable {
                table_name: new_name,
            } => {
                // The kind's Display renders "TO Gadget"; the name is inside
                // it, and using the rendered form names the table "TO Gadget".
                let target = match new_name {
                    RenameTableNameKind::To(name) | RenameTableNameKind::As(name) => name,
                };
                let new_name = object_name(target);
                let mut moved = table.clone();
                moved.name = new_name.clone();
                // Remove by the stored key rather than what the user typed:
                // the two can differ in case, which would leave the rows
                // present under both names.
                let old_key = database
                    .tables
                    .keys()
                    .find(|k| k.eq_ignore_ascii_case(&table_name))
                    .cloned();
                if let Some(key) = old_key {
                    database.tables.remove(&key);
                }
                database.tables.insert(new_name, moved);
                return Ok(Outcome::ok());
            }
            AlterTableOperation::AddConstraint { constraint, .. } => {
                apply_constraint(table, constraint)?
            }
            AlterTableOperation::DropConstraint { name, .. } => {
                table.indexes.retain(|i| i.name != name.value);
                table.foreign_keys.retain(|f| f.name != name.value);
            }
            AlterTableOperation::DropForeignKey { name, .. } => {
                table.foreign_keys.retain(|f| f.name != name.value);
            }
            AlterTableOperation::DropPrimaryKey { .. } => {
                table.indexes.retain(|i| !i.primary);
                for column in &mut table.columns {
                    column.primary_key = false;
                }
            }
            AlterTableOperation::DropIndex { name } => {
                table.indexes.retain(|i| i.name != name.value);
            }
            // sqlparser has no ALTER TABLE ... RENAME INDEX, which is one of
            // the five parser gaps the conformance survey found. Renaming a
            // constraint is the form it does parse.
            AlterTableOperation::RenameConstraint { old_name, new_name } => {
                if let Some(index) = table.indexes.iter_mut().find(|i| i.name == old_name.value) {
                    index.name = new_name.value.clone();
                }
                if let Some(fk) = table
                    .foreign_keys
                    .iter_mut()
                    .find(|f| f.name == old_name.value)
                {
                    fk.name = new_name.value.clone();
                }
            }
            AlterTableOperation::ChangeColumn {
                old_name,
                new_name,
                data_type,
                options,
                ..
            } => {
                let index = table
                    .column_index(&old_name.value)
                    .ok_or_else(|| EngineError::unknown_column(&old_name.value, &table_name))?;
                let position = table.columns[index].position;
                let def = ColumnDef {
                    name: new_name.clone(),
                    data_type: data_type.clone(),
                    options: options
                        .iter()
                        .map(|o| sqlparser::ast::ColumnOptionDef {
                            name: None,
                            option: o.clone(),
                        })
                        .collect(),
                };
                table.columns[index] = build_column(&def, position)?;
            }
            AlterTableOperation::ModifyColumn {
                col_name,
                data_type,
                options,
                ..
            } => {
                let index = table
                    .column_index(&col_name.value)
                    .ok_or_else(|| EngineError::unknown_column(&col_name.value, &table_name))?;
                let position = table.columns[index].position;
                let def = ColumnDef {
                    name: col_name.clone(),
                    data_type: data_type.clone(),
                    options: options
                        .iter()
                        .map(|o| sqlparser::ast::ColumnOptionDef {
                            name: None,
                            option: o.clone(),
                        })
                        .collect(),
                };
                table.columns[index] = build_column(&def, position)?;
            }
            // Storage engine, charset and comment changes do not alter what a
            // query returns.
            AlterTableOperation::SetTblProperties { .. }
            | AlterTableOperation::AlterColumn { .. }
            | AlterTableOperation::OwnerTo { .. } => {}
            other => return Err(unsupported(format!("ALTER TABLE operation `{other}`"))),
        }
    }

    Ok(Outcome::ok())
}

pub fn create_index(
    engine: &Engine,
    session: &mut Session,
    create: &CreateIndex,
) -> EngineResult<Outcome> {
    let (db, table_name) = engine.split_name_public(session, &create.table_name)?;
    let mut catalog = engine.catalog_mut_public()?;
    let database = catalog
        .database_mut(&db)
        .ok_or_else(|| EngineError::unknown_database(&db))?;
    let table = database
        .table_mut(&table_name)
        .ok_or_else(|| EngineError::no_such_table(&table_name))?;

    let name = create
        .name
        .as_ref()
        .map(object_name)
        .unwrap_or_else(|| format!("{table_name}_idx"));

    if create.if_not_exists && table.indexes.iter().any(|i| i.name == name) {
        return Ok(Outcome::ok());
    }

    let columns = index_column_names(&create.columns);
    let index = Index {
        name,
        columns,
        unique: create.unique,
        primary: false,
    };

    // A unique index over data that already violates it would leave the table
    // permanently inconsistent with its own constraint.
    if index.unique {
        let mut probe = table.clone();
        probe.indexes = vec![index.clone()];
        probe.rows.clear();
        for row in &table.rows {
            probe.check_unique(row, None)?;
            probe.rows.push(row.clone());
        }
    }

    table.indexes.push(index);
    Ok(Outcome::ok())
}

/// The value MySQL stores in a NOT NULL column with no declared default.
fn implicit_default(ty: &SqlType) -> Value {
    use crate::types::TypeKind;
    match ty.kind {
        TypeKind::Bool | TypeKind::Int | TypeKind::Year => Value::Int(0),
        TypeKind::Float => Value::Float(0.0),
        TypeKind::Decimal => Value::text("0"),
        TypeKind::Date => Value::text("1970-01-01"),
        TypeKind::DateTime => Value::text("1970-01-01 00:00:00"),
        TypeKind::Time => Value::text("00:00:00"),
        TypeKind::Json => Value::text("null"),
        // ENUM defaults to its first permitted value.
        TypeKind::Enum => ty
            .variants
            .first()
            .map(Value::text)
            .unwrap_or_else(|| Value::text("")),
        _ => Value::text(""),
    }
}

fn object_name(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|p| p.to_string().trim_matches('`').to_owned())
        .collect::<Vec<_>>()
        .join(".")
}
