//! Data definition: CREATE TABLE, ALTER TABLE, CREATE INDEX.

use sqlparser::ast::{
    AlterTableOperation, ColumnDef, ColumnOption, CreateIndex, CreateTable, CreateTableLikeKind,
    CreateTableOptions, ObjectName, RenameTableNameKind, SqlOption, TableConstraint,
};

use crate::catalog::{Catalog, Column, ForeignKey, Index, Table};
use crate::error::{unsupported, EngineError, EngineResult};
use crate::exec::Engine;
use crate::expr;
use crate::name;
use crate::result::{OutColumn, Outcome, ResultSet};
use crate::session::Session;
use crate::types::SqlType;
use crate::value::Value;

pub fn create_table(
    engine: &Engine,
    session: &mut Session,
    create: &CreateTable,
) -> EngineResult<Outcome> {
    // Before the name is split and before anything is looked up: 8.3 settles
    // an identifier's length while it parses, so it wins over every other
    // refusal the statement could raise. Measured there — `CREATE TABLE
    // <65 chars> AS SELECT * FROM nosuch` is 1059 and not 1146, `CREATE TABLE
    // IF NOT EXISTS <existing> (<65 chars> INT)` is 1059 and not silence, and
    // `(b INT, b INT, <65 chars> INT)` is 1059 and not 1060.
    //
    // Both parts, because the database is an identifier too: `CREATE TABLE
    // <65 chars>.t` is 1059 on 8.3 rather than the 1049 an unknown database
    // would give.
    for part in name::parts(&create.name) {
        name::check_identifier(&part)?;
    }

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
    // Through the same reader the column-level clause and the expression use.
    // This one was parsed and dropped: `create.table_options` was never read
    // and `Table::collation` never assigned, so
    // `CREATE TABLE t (a VARCHAR(10)) COLLATE=utf8mb4_bin` succeeded, reported
    // the database's collation, and compared `a` case-insensitively — the
    // exact wrong answer the column-level refusal exists to prevent, reachable
    // by writing the clause one line higher. This repo's own example
    // migrations use the table-level form on every table.
    table.collation = table_collation(&create.table_options)?;

    let mut inline_checks = Vec::new();
    for (position, def) in create.columns.iter().enumerate() {
        table.columns.push(build_column_with_checks(
            def,
            position + 1,
            &mut inline_checks,
        )?);
    }
    table.checks.append(&mut inline_checks);

    // The names the query hands over are checked too, and answered 1166 where
    // the definition's own columns are answered 1059 — measured on 8.3, both
    // for an explicit alias and for the name an unaliased expression generates
    // (`AS SELECT i+1+1+...` past 64 characters is 1166 there).
    //
    // Over the result's *columns* and not its rows, which is the shape #0009's
    // note is about: `... AS SELECT i AS <65 chars> FROM Empty WHERE 1 = 0`
    // still refuses, because a name is settled when it is resolved rather than
    // when a row is evaluated.
    if let Some(result) = &selected {
        for column in &result.columns {
            name::check_column_name(&column.name)?;
        }
    }

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
    let (unselected, coerced, database_collation) = {
        let catalog = engine.catalog_public()?;
        // Read here rather than under the write guard below, because a
        // column's collation has to be settled before any row is coerced into
        // it and that happens while this guard is the only one held.
        let database_collation = catalog
            .database(&db)
            .ok_or_else(|| EngineError::unknown_database(&db))?
            .collation
            .clone();
        let (unselected, coerced) = match &selected {
            Some(result) => merge_selected_columns(&catalog, &db, &mut table, result)?,
            None => (0, Vec::new()),
        };
        (unselected, coerced, database_collation)
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

    // A column-level UNIQUE needs a real index entry, not just the metadata
    // flag: uniqueness is enforced by walking the indexes, so without one the
    // column reports UNI in information_schema while admitting duplicates.
    //
    // Before the table-level constraints, because the order is what decides
    // which of two colliding names gets the plain one: `(a INT UNIQUE, INDEX
    // (a))` on 8.3 is a unique `a` beside a non-unique `a_2`, and this loop
    // running second would have swapped them. It is also what makes
    // `(a INT UNIQUE, INDEX a (b))` the 1061 it is on 8.3 rather than an `a`
    // and an `a_2`, which is the whole point of getting the order right.
    //
    // 8.3 follows the statement's own order, and a table constraint may be
    // written before a column — `(INDEX (a), a INT UNIQUE)` is legal there and
    // names them the other way round. The parse hands columns and constraints
    // over as two lists with the interleaving gone, so the order they were
    // written in is not recoverable here; columns first is the spelling
    // everything writes.
    //
    // The `unique` flag can only have come from a column option here, since
    // `TableConstraint::Unique` has not run yet — which is why this no longer
    // has to guess whether a table-level UNIQUE already covered the column.
    for name in table
        .columns
        .iter()
        .filter(|c| c.unique && !c.primary_key)
        .map(|c| c.name.clone())
        .collect::<Vec<_>>()
    {
        let index_name = auto_index_name(&table, std::slice::from_ref(&name));
        add_index(
            &mut table,
            Index {
                name: index_name,
                columns: vec![name],
                unique: true,
                primary: false,
            },
        )?;
    }

    for constraint in &create.constraints {
        apply_constraint(&mut table, constraint)?;
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
        add_index(
            &mut table,
            Index {
                name: "PRIMARY".to_owned(),
                columns: inline_pk,
                unique: true,
                primary: true,
            },
        )?;
    }

    // Before the rows, and that is the point: `fill_new_table` below coerces
    // every value into its column and checks the unique keys as it goes, so a
    // column whose collation had not been worked out yet was compared under
    // the default. Measured on 8.3, `CREATE TABLE d (v VARCHAR(20) COLLATE
    // utf8mb4_bin, UNIQUE KEY (v)) AS SELECT v FROM src` over `'a'` and `'A'`
    // keeps both rows; resolving afterwards made it 1062.
    resolve_collations(&mut table, &database_collation);

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
///   That width is `Item::max_length` — #0126.
///
///   `OutColumn::materialised_sql_type` models the integer half of it for a
///   view, a derived table and a CTE, and this path deliberately does not
///   share it: `CREATE TABLE ... AS SELECT` narrows by a *different* rule.
///   Measured on 8.3, a view over `mi + 1` from a `MEDIUMINT` is a `bigint`
///   and `CREATE TABLE ... AS` over the same expression is an `int`, and so
///   are `LENGTH('ab')`, `INSTR('abc','b')` and `CRC32('a')`, all of which the
///   view declares `bigint`. Borrowing the view's rule here would narrow a
///   subset correctly and still leave those wrong, so the rule this path needs
///   is its own measurement and belongs with #0126.
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
    // Recorded as though the statement had written the clause, because to the
    // table it is one: a derived column takes the collation of what produced
    // it and the table's own `COLLATE` does not reach it. Measured on 8.3 —
    // `CREATE TABLE d COLLATE=utf8mb4_general_ci AS SELECT s FROM src` over a
    // `utf8mb4_bin` column reports `utf8mb4_bin` for `d.s`, not the clause.
    // Left unrecorded, `resolve_collations` read it as a column with no clause
    // and overwrote the copied collation with the table's — while
    // `binary_collation`, which rides on the same `SqlType`, survived. The
    // column then carried the BINARY wire flag and compared case-insensitively.
    if matches!(
        ty.kind,
        crate::types::TypeKind::Text | crate::types::TypeKind::Enum | crate::types::TypeKind::Set
    ) {
        built.collation = Some(ty.collation.name().to_owned());
    }
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
    resolve_collations(&mut table, &database.collation);
    database.tables.insert(name.to_owned(), table);
    drop(catalog);
    engine.mark_modified_public(session, db, name);
    Ok(Outcome::ok())
}

/// Fills in each column's effective collation, and puts it on the rows.
///
/// Over the whole table rather than inside `build_column`, because a column's
/// collation is not a property of its own definition: `CREATE TABLE t (a
/// VARCHAR(10)) COLLATE=utf8mb4_bin` puts the table's clause on `a`, and the
/// table option is parsed after the columns are. This is the only point where
/// all three clauses — the column's, the table's, the database's — are in hand
/// at once.
///
/// Run after `ALTER TABLE` too, which is what re-tags rows already stored when
/// a `MODIFY` changes the collation under them.
pub(crate) fn resolve_collations(table: &mut Table, database_collation: &str) {
    let table_collation = table.collation.clone();
    for column in &mut table.columns {
        let name = crate::collation::inherited(
            column.collation.as_deref(),
            table_collation.as_deref(),
            database_collation,
        );
        // Every clause has been through `collation::named`, and the database's
        // is this engine's own, so an unknown name here is not reachable from
        // SQL. Falling back to the default rather than refusing keeps that
        // unreachable case from being a way to fail a `CREATE TABLE`.
        column.ty.collation = crate::collation::Collation::from_name(name).unwrap_or_default();
        // A `_bin` collation is what puts the BINARY flag on a text column's
        // wire definition — measured on 8.3, a `VARCHAR(10) COLLATE
        // utf8mb4_bin` is `VAR_STRING` with `Flags: BINARY` while the columns
        // beside it carry none.
        if column.ty.collation == crate::collation::Collation::Bin
            && matches!(
                column.ty.kind,
                crate::types::TypeKind::Text
                    | crate::types::TypeKind::Enum
                    | crate::types::TypeKind::Set
            )
        {
            column.ty.binary_collation = true;
        }
        // The stored default too, which is a value of this column and is
        // written into rows verbatim — an `ALTER TABLE ... ADD COLUMN`
        // back-fills with it, and an `INSERT` that omits the column copies it.
        // It was coerced in `build_column`, before the table's own clause had
        // been read, so it carried the wrong collation into every row that
        // took it.
        if let Some(crate::value::Value::Text(_, collated)) = &mut column.default {
            *collated = crate::collation::Collated::implicit(column.ty.collation);
        }
    }
    table.retag_text();
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
    // Before the type, because 8.3 settles the name while it parses. This is
    // the check `CREATE TABLE`, `ALTER TABLE ADD COLUMN` and `ALTER TABLE
    // CHANGE COLUMN` all reach — the three spellings that *define* a column,
    // and the three 8.3 answers 1059 for.
    name::check_identifier(&def.name.value)?;
    let declared = SqlType::from_ast(&def.data_type)?;
    declared.check_declaration_limits(&def.name.value)?;
    let mut column = Column::new(def.name.value.clone(), declared);
    column.position = position;
    // MySQL columns are nullable unless declared otherwise, except that a
    // PRIMARY KEY column is implicitly NOT NULL.
    column.nullable = true;

    for option in &def.options {
        // `CONSTRAINT <name> CHECK (...)` written against the column instead of
        // against the table. It is the same constraint under the same limit,
        // and `constraint_names` never sees it because that reads a
        // `TableConstraint` and this is a `ColumnOptionDef` -- so the spelling
        // one line lower was refused while this one was stored, over-long name
        // and all, into the catalog and the state image. Measured on 8.3, both
        // 1059: `CREATE TABLE c (i INT CONSTRAINT <65> CHECK (i > 0))` and
        // `ALTER TABLE t ADD COLUMN k INT CONSTRAINT <65> CHECK (k > 0)`.
        if let Some(name) = &option.name {
            name::check_identifier(&name.value)?;
        }
        // A declaration's expressions are stored as text here and evaluated
        // per row later, so anything 8.3 settles while it *resolves* has to be
        // settled by the CREATE or it is never settled at all — the zero-rows
        // shape of #0009, arriving through DDL rather than through a query.
        // Measured on 8.3: `CREATE TABLE U (a INT, b INT AS (SIGN(1, 2)))` is
        // 1582 there and `b INT AS (a LIKE 'b' ESCAPE '!!')` is 1210, where
        // both were accepted here — leaving the catalog, and the state image
        // with it, holding a table 8.3 could not have made.
        if let ColumnOption::Generated {
            generation_expr: Some(e),
            ..
        }
        | ColumnOption::Default(e) = &option.option
        {
            crate::exec::refuse_at_resolve([e])?;
        }
        if let ColumnOption::Check(check) = &option.option {
            crate::exec::refuse_at_resolve([check.expr.as_ref()])?;
        }
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
            // Through the one reader, which reads the name rather than its
            // rendering and lowercases it the way 8.3 does. This stored the
            // rendering, so `COLLATE `utf8mb4_general_ci`` reached
            // `information_schema.COLUMNS.COLLATION_NAME` — and the state
            // image — with its backticks still on; and it accepted any name at
            // all, which `collation::named` now checks for the reasons written
            // there.
            ColumnOption::Collation(c) => column.collation = Some(crate::collation::read(c)?),
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

/// Whether `table` already carries an index under `name`.
///
/// Case-insensitive, as MySQL compares them: `INDEX i (a), INDEX I (a)` is a
/// duplicate on 8.3.
///
/// The primary key is not in this name space. MySQL calls its index `PRIMARY`
/// whatever the constraint was named, refuses a second one with 1068
/// `Multiple primary key defined` rather than 1061, and refuses an index
/// explicitly named `PRIMARY` with 1280 `Incorrect index name` — three answers
/// this engine does not give yet, and folding them into this one would trade
/// an accepted statement for a wrongly numbered refusal.
fn index_name_taken(table: &Table, name: &str) -> bool {
    table
        .indexes
        .iter()
        .any(|i| !i.primary && i.name.eq_ignore_ascii_case(name))
}

/// How many kinds of constraint the table carries under one name.
///
/// The name space `DROP CONSTRAINT` reads is the one
/// `information_schema.TABLE_CONSTRAINTS` lists: primary key, unique, foreign
/// key and check. It is not "every index" and not "not indexes" — it is
/// *unique* indexes and the primary key, and never an ordinary non-unique one.
/// Measured on 8.3, all on fresh tables:
///
/// - `INDEX ix (a)` — `DROP CONSTRAINT ix` is 3940 and `KEY ix (a)` is still
///   there afterwards, where `DROP INDEX ix` would have taken it.
/// - `CONSTRAINT uq UNIQUE (a)`, `UNIQUE INDEX uq (a)`, `UNIQUE KEY uq (a)`,
///   and the index a column-level `a INT UNIQUE` creates under the column's
///   own name — `DROP CONSTRAINT` drops every one of them.
/// - `PRIMARY KEY (a)` — `` DROP CONSTRAINT `PRIMARY` `` drops it, and on a
///   table with no primary key the same statement is 3940. Never under the
///   constraint name it was written with: `CONSTRAINT mypk PRIMARY KEY (a)`
///   then `DROP CONSTRAINT mypk` is 3940, because 8.3 keeps no such name.
/// - the index a foreign key made for itself is non-unique, so after
///   `DROP CONSTRAINT fk1` has taken the key the leftover `KEY fk1 (pid)`
///   answers a second `DROP CONSTRAINT fk1` with 3940.
///
/// A count of *kinds* rather than a yes/no, because one name can be in two of
/// them at once and 8.3 refuses that with 3939 rather than dropping both:
/// `CONSTRAINT n UNIQUE (a), CONSTRAINT n CHECK (b > 0)` and `CONSTRAINT n
/// UNIQUE (a), CONSTRAINT n FOREIGN KEY (a) REFERENCES R(id)` are both
/// reachable there, as is a primary key beside a `` CONSTRAINT `PRIMARY`
/// CHECK (...) ``. At most one of each kind can match, since an index name, a
/// foreign key's and a check's are each unique within a table.
///
/// Kept beside `drop_constraint_named`, which must remove exactly what this
/// counts: a name this accepted and that could not remove would report success
/// having dropped nothing.
fn constraint_kinds_named(table: &Table, name: &str) -> usize {
    let unique_index = table
        .indexes
        .iter()
        .any(|i| i.unique && i.name.eq_ignore_ascii_case(name));
    let foreign_key = table
        .foreign_keys
        .iter()
        .any(|f| f.name.eq_ignore_ascii_case(name));
    let check = table
        .checks
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case(name));
    usize::from(unique_index) + usize::from(foreign_key) + usize::from(check)
}

/// Removes the one constraint `constraint_kinds_named` found under this name.
fn drop_constraint_named(table: &mut Table, name: &str) {
    drop_indexes(table, |i| i.unique && i.name.eq_ignore_ascii_case(name));
    drop_named(&mut table.foreign_keys, |f| {
        f.name.eq_ignore_ascii_case(name)
    });
    drop_named(&mut table.checks, |c| c.name.eq_ignore_ascii_case(name));
}

/// Removes every item a drop names, reporting whether it named any.
///
/// `Vec::retain` cannot say whether it removed anything, and every `DROP` here
/// has to know: the difference between having dropped something and having
/// dropped nothing is the difference between success and 1091.
fn drop_named<T>(items: &mut Vec<T>, matches: impl Fn(&T) -> bool) -> bool {
    let before = items.len();
    items.retain(|item| !matches(item));
    items.len() != before
}

/// The same, for indexes, clearing the primary-key flags along with the key.
///
/// The flags are what `information_schema.COLUMNS.COLUMN_KEY` reports from, so
/// an index removed without them leaves a column still described as `PRI`
/// after its key is gone. 8.3 reports it empty, measured through both
/// `DROP PRIMARY KEY` and `` DROP INDEX `PRIMARY` ``.
fn drop_indexes(table: &mut Table, matches: impl Fn(&Index) -> bool) -> bool {
    let was_primary = table.indexes.iter().any(|i| i.primary && matches(i));
    let dropped = drop_named(&mut table.indexes, matches);
    if was_primary {
        for column in &mut table.columns {
            column.primary_key = false;
        }
    }
    dropped
}

/// Whether the table has a primary key at all.
///
/// Both halves, because `ALTER TABLE t ADD COLUMN id INT PRIMARY KEY` sets the
/// column's flag without adding the index that `CREATE TABLE` would — a gap of
/// its own, and one that must not turn into a wrong refusal here: 8.3 accepts
/// `DROP PRIMARY KEY` after that ALTER, so reading `indexes` alone would
/// answer 1091 where MySQL succeeds.
fn has_primary_key(table: &Table) -> bool {
    table.indexes.iter().any(|i| i.primary) || table.columns.iter().any(|c| c.primary_key)
}

/// The name MySQL gives a check the statement did not name.
///
/// `<table>_chk_<n>`, where `n` is one past the highest suffix the table
/// already carries — not one past how many checks it has. The two agree until
/// a check is *dropped*, and then they do not: measured on 8.3, a table with
/// `C_chk_1` and `C_chk_2` that loses `C_chk_1` names its next check
/// `C_chk_3`, where counting would have said `C_chk_2` and put a second check
/// under a name the table already used. One `DROP CONSTRAINT C_chk_2` would
/// then have taken both away, and checks here are enforced, so that is a
/// constraint silently stopping.
///
/// Nothing could reach this before: `DROP CHECK` does not parse and
/// `DROP CONSTRAINT` did not touch `checks`, so the count only ever grew.
///
/// Derived from the names present rather than from a stored counter, which is
/// what 8.3 does too — a table whose checks have all been dropped starts again
/// at `_chk_1`, and one whose only check was *written* as `E2_chk_5` names the
/// next one `E2_chk_6`.
fn auto_check_name(table: &Table) -> String {
    let prefix = format!("{}_chk_", table.name);
    let highest = table
        .checks
        .iter()
        // `get` rather than a slice: the prefix length need not fall on a
        // character boundary of a name that merely happens to be that long.
        .filter_map(|c| Some((c.name.get(..prefix.len())?, c.name.get(prefix.len()..)?)))
        .filter(|(head, _)| head.eq_ignore_ascii_case(&prefix))
        .filter_map(|(_, suffix)| suffix.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", highest + 1)
}

/// Adds an index, refusing a name the table already carries.
///
/// An index name is scoped to its table, so the same name on two tables is
/// legal and nothing outside `table` is consulted. Measured on 8.3, where 1061
/// is the answer for a duplicate inside one CREATE TABLE, for a second CREATE
/// INDEX, and for ALTER TABLE ADD INDEX and ADD CONSTRAINT alike — including
/// the index a foreign key creates for itself, which is why
/// `ADD CONSTRAINT c FOREIGN KEY` reports 1061 and not the 1826 a constraint
/// name reused across *tables* gets.
fn add_index(table: &mut Table, index: Index) -> EngineResult<()> {
    if !index.primary && index_name_taken(table, &index.name) {
        return Err(EngineError::duplicate_key_name(&index.name));
    }
    table.indexes.push(index);
    Ok(())
}

/// The name MySQL gives an index the statement did not name.
///
/// The first column, and then `_2`, `_3` … until one is free. The suffix
/// counts candidates rather than indexes, so an explicit name already holding
/// one of them is skipped over: measured on 8.3,
/// `INDEX a_2 (b), INDEX (a), INDEX (a)` names them `a_2`, `a` and `a_3`.
///
/// The first column alone, not all of them joined: `INDEX (a, b)` is `a` there.
/// The difference is not cosmetic now that the name decides what collides —
/// `INDEX (a, b), INDEX a (b)` is 1061 on 8.3.
fn auto_index_name(table: &Table, columns: &[String]) -> String {
    let base = columns.first().cloned().unwrap_or_default();
    if !index_name_taken(table, &base) {
        return base;
    }
    // Terminates: the table holds finitely many indexes, so some suffix is free.
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{base}_{suffix}");
        if !index_name_taken(table, &candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn apply_constraint(table: &mut Table, constraint: &TableConstraint) -> EngineResult<()> {
    for written in constraint_names(constraint) {
        name::check_identifier(&written.value)?;
    }
    match constraint {
        TableConstraint::PrimaryKey(pk) => {
            let names = index_column_names(&pk.columns);
            for column in &mut table.columns {
                if names.iter().any(|n| n.eq_ignore_ascii_case(&column.name)) {
                    column.primary_key = true;
                    column.nullable = false;
                }
            }
            // `PRIMARY`, and never `pk.name`. MySQL does not keep a primary
            // key's constraint name at all: `CONSTRAINT mypk PRIMARY KEY (a)`
            // reports its index as `PRIMARY` on 8.3, and
            // `DROP CONSTRAINT mypk` afterwards is 3940 `Constraint 'mypk'
            // does not exist.` Storing it under `mypk` put the key in the name
            // space of ordinary indexes, where an `INDEX mypk (b)` beside it
            // was accepted — as it is on 8.3, whose key is not called that —
            // and a later `DROP INDEX mypk` then took the primary key with it.
            add_index(
                table,
                Index {
                    name: "PRIMARY".to_owned(),
                    columns: names,
                    unique: true,
                    primary: true,
                },
            )?;
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
            let index_name = name
                .as_ref()
                .or(unique.index_name.as_ref())
                .map(|n| n.value.clone())
                .unwrap_or_else(|| auto_index_name(table, &names));
            add_index(
                table,
                Index {
                    name: index_name,
                    columns: names,
                    unique: true,
                    primary: false,
                },
            )?;
        }
        TableConstraint::Index(index) => {
            let (columns, name) = (&index.columns, &index.name);
            let names = index_column_names(columns);
            let index_name = name
                .as_ref()
                .map(|n| n.value.clone())
                .unwrap_or_else(|| auto_index_name(table, &names));
            add_index(
                table,
                Index {
                    name: index_name,
                    columns: names,
                    unique: false,
                    primary: false,
                },
            )?;
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
            // The name is checked only where an index is actually created:
            // measured on 8.3, a foreign key whose columns an existing index
            // already covers takes no index of its own and so may reuse that
            // index's name — `INDEX b (b), INDEX fk9 (a)` accepts
            // `ADD CONSTRAINT fk9 FOREIGN KEY (b)`, while the same statement
            // against an uncovered column is 1061.
            if !covered {
                add_index(
                    table,
                    Index {
                        name: constraint_name.clone(),
                        columns: key_columns.clone(),
                        unique: false,
                        primary: false,
                    },
                )?;
            }

            table.foreign_keys.push(ForeignKey {
                name: constraint_name,
                columns: key_columns,
                referenced_table: name::dotted(foreign_table),
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
            let name = match check.name.as_ref() {
                Some(written) => written.value.clone(),
                None => auto_check_name(table),
            };
            table.checks.push(crate::catalog::Check {
                name,
                expr: check.expr.to_string(),
            });
        }
        other => return Err(unsupported(format!("table constraint `{other}`"))),
    }
    Ok(())
}

/// Refuses an `ALTER TABLE` operation that introduces a name MySQL will not
/// take.
///
/// Only the operations that *introduce* a name. One that names something the
/// table is supposed to already hold — `DROP COLUMN`, `MODIFY`, `DROP INDEX`,
/// the old half of a rename — is left to fail its lookup, which is a refusal
/// either way and is what this engine already answered.
///
/// `RENAME COLUMN` is the odd one and is measured, not assumed: 8.3 answers it
/// 1166 where `ADD COLUMN` and `CHANGE COLUMN` in the same statement position
/// are 1059.
fn check_operation_names(operation: &AlterTableOperation) -> EngineResult<()> {
    match operation {
        AlterTableOperation::AddColumn { column_def, .. } => {
            name::check_identifier(&column_def.name.value)
        }
        AlterTableOperation::ChangeColumn { new_name, .. } => {
            name::check_identifier(&new_name.value)
        }
        AlterTableOperation::RenameColumn {
            new_column_name, ..
        } => name::check_column_name(&new_column_name.value),
        AlterTableOperation::RenameTable { table_name } => {
            let (RenameTableNameKind::To(target) | RenameTableNameKind::As(target)) = table_name;
            name::parts(target)
                .iter()
                .try_for_each(|part| name::check_identifier(part))
        }
        AlterTableOperation::AddConstraint { constraint, .. } => constraint_names(constraint)
            .into_iter()
            .try_for_each(|written| name::check_identifier(&written.value)),
        AlterTableOperation::RenameConstraint { new_name, .. } => {
            name::check_identifier(&new_name.value)
        }
        _ => Ok(()),
    }
}

/// Every name a constraint *writes* for itself, whichever clause carried it.
///
/// Only the written ones. The names this module invents where a constraint has
/// none — `PRIMARY`, `<table>_ibfk_1`, `<table>_chk_1`, the columns joined by
/// underscores — are not identifiers the statement asked for, and 8.3 does not
/// refuse a table for the length of a name it made up itself.
///
/// `UNIQUE` yields two: `CONSTRAINT c UNIQUE (...)` puts the name in `name`
/// and `UNIQUE INDEX c (...)` — the spelling Prisma generates — puts it in
/// `index_name`, and reading only one of them would leave the other unchecked.
fn constraint_names(constraint: &TableConstraint) -> Vec<&sqlparser::ast::Ident> {
    let carried: [Option<&sqlparser::ast::Ident>; 2] = match constraint {
        TableConstraint::PrimaryKey(pk) => [pk.name.as_ref(), pk.index_name.as_ref()],
        TableConstraint::Unique(unique) => [unique.name.as_ref(), unique.index_name.as_ref()],
        TableConstraint::Index(index) => [index.name.as_ref(), None],
        TableConstraint::ForeignKey(fk) => [fk.name.as_ref(), fk.index_name.as_ref()],
        TableConstraint::Check(check) => [check.name.as_ref(), None],
        _ => [None, None],
    };
    carried.into_iter().flatten().collect()
}

/// The `COLLATE` a table option carries, where it carries one.
///
/// `DEFAULT CHARSET` beside it is not read: this engine has one character set,
/// and a `CHARACTER SET` clause that named another would have to be refused on
/// its own terms rather than through the collation. That is its own gap.
fn table_collation(options: &CreateTableOptions) -> EngineResult<Option<String>> {
    let list = match options {
        CreateTableOptions::Plain(list)
        | CreateTableOptions::With(list)
        | CreateTableOptions::Options(list)
        | CreateTableOptions::TableProperties(list) => list,
        CreateTableOptions::None => return Ok(None),
    };
    for option in list {
        // `COLLATE=x` and `COLLATE x` both arrive as this, with the `=` gone.
        if let SqlOption::KeyValue { key, value } = option {
            if key.value.eq_ignore_ascii_case("COLLATE") {
                return crate::collation::read_option(value).map(Some);
            }
        }
    }
    Ok(None)
}

fn index_column_names(columns: &[sqlparser::ast::IndexColumn]) -> Vec<String> {
    columns
        .iter()
        .map(|c| match &c.column.expr {
            sqlparser::ast::Expr::Identifier(ident) => ident.value.clone(),
            sqlparser::ast::Expr::CompoundIdentifier(parts) => {
                parts.last().map(|p| p.value.clone()).unwrap_or_default()
            }
            // A functional index — `KEY ((a + 1))` — indexes an expression and
            // not a column, and this engine matches an index's columns by
            // name. Rendered, it matches no column and the index does nothing,
            // which is what it did before; naming it after some column inside
            // the expression would make it silently index the wrong thing.
            other => other.to_string(),
        })
        .collect()
}

pub fn alter_table(
    engine: &Engine,
    session: &mut Session,
    name: &ObjectName,
    operations: &[AlterTableOperation],
    if_exists: bool,
) -> EngineResult<Outcome> {
    // Before the table is looked up: 8.3 answers `ALTER TABLE nosuch ADD
    // COLUMN <65 chars> INT` with 1059 rather than 1146, so the length is
    // settled while the statement is read and not while it is applied. Every
    // operation is checked before any is applied for the same reason the loop
    // below works on a copy — one statement either happens or does not.
    for operation in operations {
        check_operation_names(operation)?;
    }

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

    // `DROP CONSTRAINT` is settled ahead of every operation in the loop below,
    // whichever order they were written in: on 8.3 both `DROP COLUMN nosuch1,
    // DROP CONSTRAINT nosuch2` and the same pair reversed report the
    // constraint, where two drops from the loop report whichever came first.
    //
    // Ahead of the loop and not ahead of the whole statement — the two
    // refusals above it win on 8.3 as they do here. `ADD COLUMN <65 chars>
    // INT, DROP CONSTRAINT nosuch` is 1059 there and `ALTER TABLE nosuchtable
    // DROP CONSTRAINT nosuch` is 1146, both measured.
    //
    // Against the table as it stands rather than as the statement leaves it,
    // which is why this reads `table` before the loop has touched it:
    // `ADD CONSTRAINT c2 CHECK (b > 1), DROP CONSTRAINT c2` is 3940 on 8.3,
    // the ADD being invisible to it.
    for operation in operations {
        if let AlterTableOperation::DropConstraint {
            name, if_exists, ..
        } = operation
        {
            match constraint_kinds_named(table, &name.value) {
                // Ambiguity is refused whether or not `IF EXISTS` was written:
                // the name is there, and what is missing is which of the two
                // constraints carrying it was meant.
                0 if *if_exists => {}
                0 => return Err(EngineError::constraint_not_found(&name.value)),
                1 => {}
                _ => return Err(EngineError::multiple_constraints(&name.value)),
            }
        }
    }

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
                        // 1091 and not the 1054 a *reference* to a missing
                        // column gets. 8.3 draws the line at the operation
                        // rather than at the name: `MODIFY nosuch INT` and
                        // `RENAME COLUMN nosuch TO z` on the same table are
                        // 1054 `Unknown column 'nosuch' in 't'` there, and
                        // this one is `Can't DROP 'nosuch'`.
                        //
                        // Asked of the copy like the 1090 above, which is what
                        // makes `DROP COLUMN a, DROP COLUMN a` 1091 on the
                        // second `a` — 8.3 answers that too.
                        None => return Err(EngineError::cant_drop(&column_name.value)),
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
                //
                // This arm writes to the catalog and returns, so it is the one
                // place the copy-then-swap rule above is bypassed and every
                // operation written *after* the rename is skipped in silence.
                // 8.3 disagrees — `RENAME TO T4b, DROP INDEX nosuch` is 1091
                // there and leaves the table called `T4` — so this is a gap of
                // its own, and not one the drop refusals below can close from
                // where they sit. Reordered the other way it now agrees:
                // `DROP INDEX nosuch, RENAME TO T4b` refuses before the rename
                // is reached.
                let target = match new_name {
                    RenameTableNameKind::To(name) | RenameTableNameKind::As(name) => name,
                };
                let new_name = name::dotted(target);
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
            // The three drops below and the rename under them match the name
            // case-insensitively, as every other identifier comparison here
            // does and as MySQL does: `DROP INDEX IX` drops `ix` on 8.3, and
            // so do `DROP FOREIGN KEY FK1` and `DROP CONSTRAINT FK1`.
            // Matching exactly made a drop under another spelling a silent
            // no-op, which the duplicate-name check turned into a refusal —
            // `DROP INDEX I, ADD INDEX i (a)` dropped nothing and then
            // collided with what it had failed to remove.
            //
            // The name that matches nothing is 1091 on all three, and
            // `drop_named` reporting whether it took anything is how that is
            // noticed: `retain` alone cannot say. Each is asked of the copy,
            // so `DROP INDEX ix, DROP INDEX ix` is 1091 on the second — 8.3
            // answers that, and the doubled `DROP FOREIGN KEY` too.
            //
            // `DROP CONSTRAINT` is the exception below, on both counts.
            AlterTableOperation::DropConstraint { name, .. } => {
                // No count and no refusal: the pre-pass owns both, having
                // already answered 3940 for a name in none of the constraint
                // name spaces and 3939 for one in two of them. That is also
                // why `DROP CONSTRAINT ck1, DROP CONSTRAINT ck1` is accepted
                // where the doubled drops below are not — the pre-pass asks
                // the table as it stands, so both see `ck1`.
                drop_constraint_named(table, &name.value);
            }
            AlterTableOperation::DropForeignKey { name, .. } => {
                if !drop_named(&mut table.foreign_keys, |f| {
                    f.name.eq_ignore_ascii_case(&name.value)
                }) {
                    return Err(EngineError::cant_drop(&name.value));
                }
            }
            AlterTableOperation::DropPrimaryKey { .. } => {
                // Asked of `has_primary_key` rather than of what the drop
                // removed, because a primary key added by `ALTER TABLE ... ADD
                // COLUMN id INT PRIMARY KEY` reaches the column's flag without
                // reaching `indexes`. 8.3 accepts `DROP PRIMARY KEY` after
                // that ALTER, so counting removed indexes would refuse a
                // statement MySQL answers.
                if !has_primary_key(table) {
                    // The literal `PRIMARY` and not the table's name: the
                    // statement wrote no name at all, and 8.3 reports the one
                    // it keeps a primary key's index under.
                    return Err(EngineError::cant_drop("PRIMARY"));
                }
                drop_indexes(table, |i| i.primary);
                // Again, for the flag-only case above, which `drop_indexes`
                // cannot have cleared because it found no index to remove.
                for column in &mut table.columns {
                    column.primary_key = false;
                }
            }
            AlterTableOperation::DropIndex { name } => {
                if !drop_indexes(table, |i| i.name.eq_ignore_ascii_case(&name.value)) {
                    return Err(EngineError::cant_drop(&name.value));
                }
            }
            // sqlparser has no ALTER TABLE ... RENAME INDEX, which is one of
            // the five parser gaps the conformance survey found — and no
            // MySQL-dialect `RENAME CONSTRAINT` either, so this arm is
            // unreachable as things stand: `RenameConstraint` is gated behind
            // the PostgreSQL dialect there, and everything here parses with
            // `MySqlDialect`. `RENAME INDEX` is instead intercepted before the
            // parser, in `rewrite.rs`, and executed in `exec.rs` — which is
            // where its 1176 for a name the table has not got lives.
            AlterTableOperation::RenameConstraint { old_name, new_name } => {
                if let Some(index) = table
                    .indexes
                    .iter_mut()
                    .find(|i| i.name.eq_ignore_ascii_case(&old_name.value))
                {
                    index.name = new_name.value.clone();
                }
                if let Some(fk) = table
                    .foreign_keys
                    .iter_mut()
                    .find(|f| f.name.eq_ignore_ascii_case(&old_name.value))
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
        resolve_collations(table, &database.collation);
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

    // After the table is found and not before, which is where 8.3 puts it:
    // `CREATE INDEX <65 chars> ON nosuch (i)` is 1146 there, where the same
    // over-long name reached through `ALTER TABLE nosuch ADD INDEX` is 1059.
    // The two paths really do disagree, so neither one is a guess about the
    // other.
    if let Some(written) = create.name.as_ref() {
        for part in name::parts(written) {
            name::check_identifier(&part)?;
        }
    }

    let name = create
        .name
        .as_ref()
        .map(name::dotted)
        .unwrap_or_else(|| format!("{table_name}_idx"));

    if create.if_not_exists && index_name_taken(table, &name) {
        return Ok(Outcome::ok());
    }

    let columns = index_column_names(&create.columns);
    let index = Index {
        name,
        columns,
        unique: create.unique,
        primary: false,
    };

    // Before the data is walked, which is the order 8.3 answers in: a
    // `CREATE UNIQUE INDEX` under a name the table already carries, over a
    // column that already holds duplicates, is 1061 there and not the 1062 the
    // rows would raise. A duplicate name also beats a column that does not
    // exist at all, so nothing about what the index covers can be the first
    // answer.
    if index_name_taken(table, &index.name) {
        return Err(EngineError::duplicate_key_name(&index.name));
    }

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

    // Through `add_index`, whose check the statement has already passed, so
    // that it stays the only place an index is appended: a name invented
    // between here and there would otherwise reach the table unchecked.
    add_index(table, index)?;
    Ok(Outcome::ok())
}
