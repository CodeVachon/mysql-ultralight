//! Data definition: CREATE TABLE, ALTER TABLE, CREATE INDEX.

use sqlparser::ast::{
    AlterTableOperation, ColumnDef, ColumnOption, CreateIndex, CreateTable, CreateTableLikeKind,
    ObjectName, RenameTableNameKind, TableConstraint,
};

use crate::catalog::{Catalog, Column, ForeignKey, Index, Table};
use crate::error::{unsupported, EngineError, EngineResult};
use crate::exec::Engine;
use crate::expr;
use crate::result::{OutColumn, Outcome, ResultSet};
use crate::session::Session;
use crate::types::SqlType;
use crate::value::Value;

pub fn create_table(
    engine: &Engine,
    session: &mut Session,
    create: &CreateTable,
) -> EngineResult<Outcome> {
    let (db, name) = engine.split_name_public(session, &create.name)?;

    if let Some(like) = &create.like {
        return create_table_like(engine, session, create, &db, &name, like);
    }

    // Run before the catalog is locked and before the name is checked: 8.3
    // resolves the query first, so `CREATE TABLE IF NOT EXISTS t AS SELECT *
    // FROM nope` is 1146 even where `t` already exists.
    let selected = match &create.query {
        Some(query) => Some(engine.query_public(session, query)?),
        None => None,
    };

    let mut table = Table::new(&name);

    let mut inline_checks = Vec::new();
    for (position, def) in create.columns.iter().enumerate() {
        table.columns.push(build_column_with_checks(
            def,
            position + 1,
            &mut inline_checks,
        )?);
    }
    table.checks.append(&mut inline_checks);

    // A table with no columns is not a table MySQL can make — `CREATE TABLE t
    // ()` is a syntax error there — and it is not one this engine can *answer*
    // either: a result set has no encoding for zero columns, so `SELECT *` over
    // one desynchronises the connection. `sqlparser` accepts the empty list, so
    // the refusal has to be here.
    if table.columns.is_empty() && selected.is_none() {
        return Err(EngineError::parse(format!(
            "Table '{name}' must have at least one column"
        )));
    }

    // A read guard rather than the write one, and dropped before the rows are
    // filled in: a CHECK constraint or an expression default may hold a
    // subquery, which runs against the catalog, and holding the writer's lock
    // across that would deadlock the statement against itself.
    let (unselected, coerced) = {
        let catalog = engine.catalog_public()?;
        match &selected {
            Some(result) => merge_selected_columns(&catalog, &db, &mut table, result)?,
            None => (0, Vec::new()),
        }
    };

    // Two columns of one name is not a table: every reference to the name
    // would be answered with the first of them. `CREATE TABLE t (a INT, a
    // INT)`, `AS SELECT id, id` and `AS SELECT * FROM src a JOIN src b` are
    // all 1060 on 8.3, and the check lived on the `AS SELECT` path alone until
    // a review found the plain spelling still made the table.
    //
    // Before the constraints and not after: 8.3 answers `(a INT, a INT,
    // PRIMARY KEY (nope))` with 1060 and not the 1072 the key would raise, so
    // a check that ran after `apply_constraint` would report the second
    // problem in a statement that has two. The comparison is case-insensitive
    // and the name reported is the later spelling, both measured — `(a INT, A
    // INT)` is `Duplicate column name 'A'`.
    for (index, column) in table.columns.iter().enumerate() {
        if table.columns[..index]
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(&column.name))
        {
            return Err(EngineError::new(
                1060,
                "42S21",
                format!("Duplicate column name '{}'", column.name),
            ));
        }
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

    // The first AUTO_INCREMENT value the rows generated, which is what
    // `LAST_INSERT_ID()` answers after a CTAS — measured on 8.3, where a
    // two-row one reports 1 rather than 2.
    let mut generated_id = None;

    // Filled before the catalog is locked for writing, and before the name is
    // claimed: every row-level rule the table declares applies here, and a row
    // that fails one refuses the whole statement — which is what 8.3 does, and
    // it leaves no table behind because nothing has been written yet.
    if let Some(result) = selected {
        let fk_parents = {
            let catalog = engine.catalog_public()?;
            let database = catalog
                .database(&db)
                .ok_or_else(|| EngineError::unknown_database(&db))?;
            crate::foreign_keys::snapshot(database, &table)
        };
        generated_id = engine.fill_new_table(
            session,
            &mut table,
            fk_parents,
            unselected,
            &coerced,
            result.rows,
        )?;
    }

    let mut catalog = engine.catalog_mut_public()?;
    let database = catalog
        .database_mut(&db)
        .ok_or_else(|| EngineError::unknown_database(&db))?;

    // A view occupies the same namespace; without this check the new table
    // would be permanently unreachable, since a view resolves first.
    if database.table(&name).is_some() || database.view(&name).is_some() {
        if create.if_not_exists {
            return Ok(Outcome::ok());
        }
        return Err(EngineError::table_exists(&name));
    }

    // The count `AS SELECT` reports is the rows it inserted, which is what 8.3
    // says: `CREATE TABLE t AS SELECT * FROM src` over two rows is
    // "Query OK, 2 rows affected". A plain CREATE TABLE reports none.
    let outcome = match create.query {
        Some(_) => Outcome::Affected {
            rows: table.rows.len() as u64,
            last_insert_id: generated_id.unwrap_or(0),
        },
        None => Outcome::ok(),
    };

    database.tables.insert(name.clone(), table);
    drop(catalog);
    engine.mark_modified_public(session, &db, &name);
    // `LAST_INSERT_ID()` keeps its previous value when nothing was generated,
    // the same rule INSERT follows.
    if let Some(id) = generated_id {
        session.last_insert_id = id;
    }
    Ok(outcome)
}

/// Adds the columns a query's result declared to a table being created, and
/// returns the rows to fill it with.
///
/// MySQL builds a `CREATE TABLE ... AS SELECT` out of the result *metadata*
/// rather than out of the statement, which is why `describe` already answers
/// the hard half of this. Measured against 8.3 with `SHOW CREATE TABLE`, over
/// `src(id INT NOT NULL AUTO_INCREMENT PRIMARY KEY, name VARCHAR(40) NOT NULL
/// DEFAULT 'x', qty INT UNSIGNED DEFAULT 7, note TEXT, UNIQUE KEY uk_name)`:
///
/// | query | declared |
/// |---|---|
/// | `SELECT * FROM src` | `id int NOT NULL DEFAULT '0'`, `name varchar(40) NOT NULL DEFAULT 'x'`, `qty int unsigned DEFAULT '7'`, `note text` |
/// | `SELECT id+1 AS a, name FROM src` | `a bigint NOT NULL DEFAULT '0'`, `name varchar(40) NOT NULL DEFAULT 'x'` |
/// | `SELECT COUNT(*) c, SUM(qty) s, AVG(amt) av, MAX(name) m FROM src` | `c bigint NOT NULL`, `s decimal(32,0)`, `av decimal(14,6)`, `m varchar(40)` |
/// | `SELECT a.id, b.name bname FROM src a LEFT JOIN src b ON ...` | `id int NOT NULL DEFAULT '0'`, `bname varchar(40) DEFAULT 'x'` |
/// | `SELECT NULL AS a` and `SELECT NULL UNION SELECT NULL` | `a binary(0) DEFAULT NULL` |
/// | `SELECT id AS v FROM src UNION SELECT name FROM src` | `v varchar(40) NOT NULL DEFAULT ''` |
///
/// So: the type, the name and the nullability are the ones the result set was
/// described with, and everything else about a copied column is dropped —
/// AUTO_INCREMENT, the primary key and every other index are gone, which is
/// what separates this from `LIKE`. The one thing that survives is the
/// DEFAULT, and only for a column read straight out of a stored table: through
/// a view the same column comes back `varchar(40) NOT NULL DEFAULT ''`, the
/// type's implicit default, because a view does not carry one. A column that
/// is NOT NULL and has no default to copy gets that implicit default too,
/// which is where `int NOT NULL DEFAULT '0'` above comes from — a plain
/// `CREATE TABLE` would leave it with none.
///
/// Two divergences, both recorded rather than guessed at:
///
/// - MySQL narrows a computed column to the width its expression can produce,
///   so `SELECT 1` declares `int` where this engine declares `bigint`, and
///   `SELECT 'x'` declares `varchar(1)` where this declares `varchar(512)`.
///   That width is `Item::max_length`, which this engine does not model at
///   all — #0126 — and inventing one per expression here would be guessing.
///   Only the integer case is visible on the wire, as LONGLONG for LONG.
/// - A default is copied out of the table the *relation* says the column came
///   from, which the alias does not change: `SELECT s.name FROM src s` and
///   `SELECT Other2.name FROM Src Other2` both copy `src`'s `'x'`, as 8.3
///   does. One projection still cannot: the grouped path has the columns
///   without the relation behind them, so `AS SELECT name FROM src GROUP BY
///   name` takes the implicit default where 8.3 copies `'x'`. A missing
///   default rather than a wrong one. See `OutColumn::origin_table`.
fn merge_selected_columns(
    catalog: &Catalog,
    database: &str,
    table: &mut Table,
    result: &ResultSet,
) -> EngineResult<(usize, Vec<bool>)> {
    // An explicit definition whose name matches one of the query's columns
    // describes *that* column, in the query's position rather than its own:
    // 8.3 answers `CREATE TABLE t (name VARCHAR(5)) AS SELECT id, name FROM
    // src` with `id` first and `name varchar(5)` second. Explicit columns the
    // query does not name keep their own place, in front of all of it.
    let mut selected = Vec::with_capacity(result.columns.len());
    // Whether each of them can hold what the query computed without being told
    // to. A copied column, and one the statement declared itself, both have a
    // type the value already fits. A column derived from an expression has
    // only the widening the wire type implies — `decimal` for what MySQL calls
    // `decimal(14,6)` — and coercing `AVG(amt)` into *that* rounds 2.000000 to
    // 2. MySQL's derived column was built to hold the value exactly, so the
    // faithful thing is to store what was computed rather than to narrow it.
    let mut coerced = Vec::with_capacity(result.columns.len());
    for column in &result.columns {
        match table
            .columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(&column.name))
        {
            Some(index) => {
                selected.push(table.columns.remove(index));
                coerced.push(true);
            }
            None => {
                selected.push(derived_column(catalog, database, column));
                coerced.push(column.declared.is_some());
            }
        }
    }
    let unselected = table.columns.len();
    table.columns.append(&mut selected);

    for (index, column) in table.columns.iter_mut().enumerate() {
        column.position = index + 1;
    }

    // An explicit column the query does not fill is a column omitted from an
    // INSERT, and takes whatever that would give it — its default, its
    // expression default, or a generated or AUTO_INCREMENT value. Working that
    // out here would be a second copy of the rule; `fill_new_table` applies
    // the first one.
    Ok((unselected, coerced))
}

/// One column of a `CREATE TABLE ... AS SELECT`, from the result column the
/// query described. See `merge_selected_columns` for what was measured.
fn derived_column(catalog: &Catalog, database: &str, column: &OutColumn) -> Column {
    let ty = column.sql_type();
    let mut built = Column::new(column.name.clone(), ty.clone());
    built.nullable = column.nullable;
    // By the *source* column's name and the *source* table, neither of which
    // an alias changes.
    //
    // Both halves were needed. Looking the column up by the result column's
    // name found a different column of the same name: `CREATE TABLE t AS
    // SELECT id AS name FROM src` copied `src.name`'s `VARCHAR` default `'x'`
    // into an `INT` column, so a later insert stored the string `'x'` there
    // and `name + 1` read it as 1. 8.3 declares `name int NOT NULL DEFAULT
    // '0'` — the source field's attributes under the alias's name.
    //
    // Looking the *table* up by `column.table` was the same mistake one level
    // out: that is the alias where the FROM clause gave one, so `SELECT
    // Other2.name FROM Src Other2` read `Other2`'s default where 8.3 copies
    // `Src`'s — a wrong answer, and one an added type check only narrowed,
    // since the shadowing table's column can have the same type. The relation
    // knows which table an alias stands for, and now says so through
    // `OutColumn::origin_table`.
    //
    // The type check stays, for the case neither name settles: a column read
    // through a widening — a union branch, say — is not the source column any
    // more, and 8.3 gives those the implicit default too.
    let copied = column
        .origin
        .as_deref()
        .zip(column.origin_table.as_deref())
        .and_then(|(source_name, source_table)| {
            catalog
                .database(if column.schema.is_empty() {
                    database
                } else {
                    &column.schema
                })
                .and_then(|db| db.table(source_table))
                .and_then(|table| table.column(source_name))
        })
        .filter(|source| source.ty == ty)
        .and_then(|source| source.default.clone());
    built.default = match (copied, built.nullable) {
        (Some(value), _) => Some(value),
        (None, false) => Some(built.ty.implicit_default()),
        (None, true) => None,
    };
    built
}

/// `CREATE TABLE ... LIKE`: the source table's whole definition and none of
/// its rows.
///
/// The opposite of `AS SELECT` in what it keeps. Measured on 8.3: the copy has
/// the source's AUTO_INCREMENT, its primary key and its other indexes, and its
/// defaults and charsets — everything `SHOW CREATE TABLE` prints — except the
/// foreign keys, which are dropped. The index a foreign key created is *not*
/// dropped with it: `CONSTRAINT fk1 FOREIGN KEY (sid)` leaves `KEY fk1 (sid)`
/// behind in the copy, which is why the indexes are taken as they stand rather
/// than rebuilt from what remains.
fn create_table_like(
    engine: &Engine,
    session: &mut Session,
    create: &CreateTable,
    db: &str,
    name: &str,
    like: &CreateTableLikeKind,
) -> EngineResult<Outcome> {
    let (CreateTableLikeKind::Plain(like) | CreateTableLikeKind::Parenthesized(like)) = like;
    if like.defaults.is_some() {
        return Err(unsupported(format!("CREATE TABLE ... {like}")));
    }
    let (source_db, source_name) = engine.split_name_public(session, &like.name)?;
    // Checked before anything is looked up, which is the order 8.3 checks it
    // in: `CREATE TABLE nosuch LIKE nosuch` is 1066 and not 1146, and so is
    // the same statement under IF NOT EXISTS. Only the resolved identity
    // counts — `CREATE TABLE probe.src LIKE c1.src` is a different table and
    // answers 1050 for the one already there.
    if source_db.eq_ignore_ascii_case(db) && source_name.eq_ignore_ascii_case(name) {
        return Err(EngineError::new(
            1066,
            "42000",
            format!("Not unique table/alias: '{name}'"),
        ));
    }

    let mut catalog = engine.catalog_mut_public()?;
    let source = catalog
        .database(&source_db)
        .ok_or_else(|| EngineError::unknown_database(&source_db))?;
    // A view is in the same namespace and is not a base table; 8.3 answers
    // 1347 rather than copying the shape it resolved to.
    if source.table(&source_name).is_none() && source.view(&source_name).is_some() {
        return Err(EngineError::new(
            1347,
            "HY000",
            format!("'{source_db}.{source_name}' is not BASE TABLE"),
        ));
    }
    let source = source
        .table(&source_name)
        .ok_or_else(|| EngineError::no_such_table(&source_name))?;

    let mut table = source.clone();
    table.name = name.to_owned();
    table.rows.clear();
    // Not copied, and the constraint is not copied either: an `_ibfk_` name is
    // unique per schema, so carrying one over would give two tables the same
    // constraint name.
    table.foreign_keys.clear();
    // The counter starts over rather than resuming the source's: the copy has
    // no rows, so a first insert taking the source's next id would leave a gap
    // that MySQL does not have.
    table.next_auto_increment = 1;

    let database = catalog
        .database_mut(db)
        .ok_or_else(|| EngineError::unknown_database(db))?;
    if database.table(name).is_some() || database.view(name).is_some() {
        if create.if_not_exists {
            return Ok(Outcome::ok());
        }
        return Err(EngineError::table_exists(name));
    }
    database.tables.insert(name.to_owned(), table);
    drop(catalog);
    engine.mark_modified_public(session, db, name);
    Ok(Outcome::ok())
}

fn build_column(def: &ColumnDef, position: usize) -> EngineResult<Column> {
    let mut inline_checks = Vec::new();
    build_column_with_checks(def, position, &mut inline_checks)
}

/// Builds a column, collecting any inline CHECK into `inline_checks`.
fn build_column_with_checks(
    def: &ColumnDef,
    position: usize,
    inline_checks: &mut Vec<crate::catalog::Check>,
) -> EngineResult<Column> {
    let declared = SqlType::from_ast(&def.data_type)?;
    declared.check_declaration_limits(&def.name.value)?;
    let mut column = Column::new(def.name.value.clone(), declared);
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
            // Coerced here rather than where it is used, so it is stored once
            // in the shape the column holds and checked once against it: a
            // `JSON DEFAULT ('{')` is refused by the CREATE that declares it
            // rather than by the first read of a row that took it.
            ColumnOption::Default(e) => match expr::literal_of(e) {
                Some(v) => column.default = Some(column.ty.coerce(v, &column.name)?),
                // Anything not a constant is stored as text and evaluated per
                // row, so CURRENT_TIMESTAMP differs between inserts.
                None => column.default_expr = Some(e.to_string()),
            },
            // An inline CHECK is the same constraint as a table-level one,
            // just written next to the column.
            ColumnOption::Check(check) => {
                inline_checks.push(crate::catalog::Check {
                    name: check
                        .name
                        .as_ref()
                        .map(|n| n.value.clone())
                        .unwrap_or_else(|| format!("{}_chk", def.name.value)),
                    expr: check.expr.to_string(),
                });
            }
            ColumnOption::Comment(c) => column.comment = Some(c.clone()),
            ColumnOption::Collation(c) => column.collation = Some(c.to_string()),
            ColumnOption::OnUpdate(e) => {
                // Only CURRENT_TIMESTAMP is meaningful here; anything else
                // would be a value this engine would have to invent.
                let rendered = e.to_string().to_ascii_lowercase();
                if rendered.starts_with("current_timestamp") || rendered.starts_with("now") {
                    column.on_update_now = true;
                } else {
                    return Err(unsupported(format!("ON UPDATE `{e}`")));
                }
            }
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
            ColumnOption::Generated {
                generation_expr, ..
            } => match generation_expr {
                Some(e) => {
                    column.generated = Some(e.to_string());
                    // A generated column has no stored default and cannot be
                    // written to directly.
                    column.default = None;
                }
                None => {
                    return Err(unsupported(format!(
                        "generated column `{}` without an expression",
                        def.name.value
                    )))
                }
            },
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
            // `CONSTRAINT c UNIQUE (...)` puts the name in `name`, while
            // `UNIQUE INDEX c (...)` — the spelling Prisma generates — puts it
            // in `index_name`. Reading only the first left every inline unique
            // index stored under an invented name, which DROP INDEX and
            // information_schema then could not find.
            table.indexes.push(Index {
                name: name
                    .as_ref()
                    .or(unique.index_name.as_ref())
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
            // MySQL numbers unnamed foreign keys `<table>_ibfk_1`, `_2`, and
            // so on. A single `<table>_fk` for all of them collides, and since
            // the implicit index takes the same name, dropping one index would
            // drop every one of them.
            let constraint_name = name
                .as_ref()
                .map(|n| n.value.clone())
                .unwrap_or_else(|| format!("{}_ibfk_{}", table.name, table.foreign_keys.len() + 1));
            let key_columns: Vec<String> = columns.iter().map(|c| c.value.clone()).collect();

            // MySQL creates an index for a foreign key unless one already
            // covers its columns, and names it after the constraint. Prisma
            // then renames that index in a later migration, which has nothing
            // to find if the index was never created.
            let covered = table
                .indexes
                .iter()
                .any(|i| i.columns.starts_with(key_columns.as_slice()));
            if !covered {
                table.indexes.push(Index {
                    name: constraint_name.clone(),
                    columns: key_columns.clone(),
                    unique: false,
                    primary: false,
                });
            }

            table.foreign_keys.push(ForeignKey {
                name: constraint_name,
                columns: key_columns,
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
        TableConstraint::Check(check) => {
            table.checks.push(crate::catalog::Check {
                name: check
                    .name
                    .as_ref()
                    .map(|n| n.value.clone())
                    .unwrap_or_else(|| format!("{}_chk_{}", table.name, table.checks.len() + 1)),
                expr: check.expr.to_string(),
            });
        }
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
    engine.mark_modified_public(session, &db, &table_name);
    let mut catalog = engine.catalog_mut_public()?;
    let database = catalog
        .database_mut(&db)
        .ok_or_else(|| EngineError::unknown_database(&db))?;

    let Some(stored) = database.table(&table_name) else {
        if if_exists {
            return Ok(Outcome::ok());
        }
        return Err(EngineError::no_such_table(&table_name));
    };

    // Applied to a copy and swapped in only once every operation has
    // succeeded. `ALTER TABLE` is one statement to MySQL however many
    // operations it lists, and this loop mutated the stored table in place, so
    // an operation that failed left the ones before it applied: `ALTER TABLE t
    // DROP COLUMN a, DROP COLUMN b` on a two-column table answered 1090 and
    // dropped `a` and its data anyway. That is the whole class rather than one
    // guard — a duplicate column name, an unknown one, an unsupported type,
    // and the last-column refusal below all reach it — so the copy closes it
    // once for all of them.
    let mut table = stored.clone();
    let table = &mut table;

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
                    (None, false) => column.ty.implicit_default(),
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
                            // The other route to a table with no columns, and
                            // the one that does not go through CREATE at all.
                            // 8.3 refuses it with 1090 rather than leaving a
                            // table `SELECT *` cannot be answered for — a
                            // zero-column result set has no encoding on the
                            // wire, so it would desynchronise the connection.
                            //
                            // Asked of the copy, so the answer is about the
                            // table as this statement has left it so far: the
                            // last column of `DROP COLUMN a, DROP COLUMN b` is
                            // only the last one because the first drop already
                            // happened. Nothing has reached the catalog yet,
                            // so refusing here undoes both.
                            if table.columns.len() == 1 {
                                return Err(EngineError::new(
                                    1090,
                                    "42000",
                                    "You can't delete all columns with ALTER TABLE; \
                                     use DROP TABLE instead",
                                ));
                            }
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
                // From the copy, so any operation earlier in the same
                // statement travels with it rather than being dropped on the
                // floor with the original.
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

    // Every operation succeeded, so the copy becomes the table. Written back
    // under the stored key rather than what the user typed, which can differ
    // in case — inserting under the typed spelling would leave the rows under
    // both names, which is the bug `RenameTable` already carries a note about.
    let key = database
        .tables
        .keys()
        .find(|k| k.eq_ignore_ascii_case(&table_name))
        .cloned();
    if let Some(key) = key {
        database.tables.insert(key, table.clone());
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

fn object_name(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|p| p.to_string().trim_matches('`').to_owned())
        .collect::<Vec<_>>()
        .join(".")
}
