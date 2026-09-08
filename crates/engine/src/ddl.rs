//! Data definition: CREATE TABLE, ALTER TABLE, CREATE INDEX.

use sqlparser::ast::{
    AlterTableOperation, ColumnDef, ColumnOption, CreateIndex, CreateTable, CreateTableLikeKind,
    CreateTableOptions, ObjectName, RenameTableNameKind, SqlOption, TableConstraint,
};

use crate::catalog::{Catalog, Column, Database, ForeignKey, Index, Table};
use crate::error::{unsupported, EngineError, EngineResult};
use crate::exec::Engine;
use crate::expr;
use crate::name;
use crate::result::{OutColumn, Outcome, ResultSet};
use crate::session::Session;
use crate::types::SqlType;
use crate::value::Value;

/// The referential action a constraint that wrote none carries.
///
/// See where it is stored: `RESTRICT` is what InnoDB *does* with it, and
/// `NO ACTION` is what `information_schema` says about it.
const NO_ACTION: &str = "NO ACTION";

/// The infix MySQL builds an unnamed foreign key's name around, between the
/// table's name and the constraint's number.
///
/// A rename rewrites the name around it — see [`rename_own_auto_keys`].
const IBFK: &str = "_ibfk_";

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
    // One at a time, because each name is worked out from the ones already
    // there: 8.3 numbers a table's generated check names in declaration order.
    for mut check in inline_checks {
        if check.name.is_empty() {
            check.name = auto_check_name(&table);
        }
        table.checks.push(check);
    }

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
            return Err(EngineError::duplicate_column(&column.name));
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
                implicit_for_foreign_key: false,
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
                implicit_for_foreign_key: false,
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

    // The same scope check `ALTER TABLE` makes, for the constraints a
    // `CREATE TABLE` declares inline. #0257.
    refuse_duplicate_constraint_names(database, &name, &table)?;

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
    // The indexes keep their marks, constraints or no constraints. It reads
    // backwards — `LIKE` copies no foreign keys, so nothing in the copy is
    // standing in for one — and clearing them was tried and measured wrong:
    // `CREATE TABLE L2 LIKE L` where `L`'s index was made for a key, and then
    // `CREATE INDEX z ON L2 (r)` with no key on `L2` at all, leaves 8.3 holding
    // `z` alone. The same copy of an index that was written by `CREATE INDEX`
    // keeps both. So the mark is a property of how the index came to exist,
    // and it survives being copied.
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
    // Every check in the copy is renamed after the copy, whatever it was
    // called in the source. Measured on 8.3, and it regenerates the
    // *explicitly* named ones too: `CREATE TABLE L (a INT, CONSTRAINT named
    // CHECK (a > 0))` copied to `L2` leaves `L2` holding `L2_chk_1`, not
    // `named`. Without this the copy carried names the database already held
    // and `LIKE` refused any source table that had a check at all.
    let copied = std::mem::take(&mut table.checks);
    for mut check in copied {
        check.name = auto_check_name(&table);
        table.checks.push(check);
    }
    refuse_duplicate_constraint_names(database, name, &table)?;
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
            crate::exec::refuse_at_resolve(None, &[], [e])?;
        }
        if let ColumnOption::Check(check) = &option.option {
            crate::exec::refuse_at_resolve(None, &[], [check.expr.as_ref()])?;
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
            //
            // Through `store` and not `coerce`, so a default the column cannot
            // hold is caught here too — and reported as 1067 rather than as
            // the 1406 or 1264 a write would get, which is what 8.3 answers:
            // `CREATE TABLE t (s VARCHAR(3) DEFAULT 'abcdef')` is `Invalid
            // default value for 's'` there, with the 1406 left behind in the
            // warning list.
            ColumnOption::Default(e) => match expr::literal_of(e) {
                Some(v) => {
                    let stored = column.ty.store(v, &column.name).map_err(|refused| {
                        // Only the two a *width* raises. Everything else the
                        // column can refuse — a document that is not JSON, a
                        // value that is not an ENUM member — keeps the code it
                        // came with, which is the code 8.3 answers for it.
                        if !matches!(refused.code, 1406 | 1264) {
                            return refused;
                        }
                        EngineError::new(
                            1067,
                            "42000",
                            format!("Invalid default value for '{}'", column.name),
                        )
                    })?;
                    column.default = Some(stored);
                }
                // Anything not a constant is stored as text and evaluated per
                // row, so CURRENT_TIMESTAMP differs between inserts.
                None => column.default_expr = Some(e.to_string()),
            },
            // An inline CHECK is the same constraint as a table-level one,
            // just written next to the column.
            ColumnOption::Check(check) => {
                inline_checks.push(crate::catalog::Check {
                    // Left empty where the statement wrote no name, and
                    // filled in by the caller: 8.3 names these after the
                    // *table* and numbers them across it — `t3_chk_1`,
                    // `t3_chk_2` — which a function looking at one column
                    // cannot do. Naming it `<column>_chk` here was merely a
                    // wrong name until a constraint name had to be unique in
                    // its database, and then it was a wrong refusal: two
                    // tables with the same inline check on the same column
                    // name collided.
                    name: check
                        .name
                        .as_ref()
                        .map(|n| n.value.clone())
                        .unwrap_or_default(),
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

/// Takes a dropped column out of every index over it.
///
/// 8.3 *narrows* rather than drops, which is the half of this that is easy to
/// get wrong: with `INDEX ix (a, b)`, dropping `a` leaves `ix (b)` there —
/// renumbered from `SEQ_IN_INDEX` 1 — and a narrowed `UNIQUE KEY` stays
/// unique, `UNIQUE uq (a, b)` minus `b` being `uq (a)` with `NON_UNIQUE` 0.
/// A `PRIMARY KEY (a, b)` narrows the same way. Only an index left with no
/// columns at all goes, which is what a single-column key does; the
/// primary-key flags go with it for the reason [`drop_indexes`] gives, though
/// here the flagged column is itself being removed.
///
/// Renumbering is not code: `information_schema.STATISTICS` reads the position
/// out of the `Vec`, so removing the element is the renumbering.
fn narrow_indexes(table: &mut Table, dropped: &str) {
    for index in &mut table.indexes {
        index.columns.retain(|c| !c.eq_ignore_ascii_case(dropped));
    }
    drop_indexes(table, |i| i.columns.is_empty());
    // A unique key that narrows onto one column *is* a one-column unique key,
    // and `information_schema.COLUMNS.COLUMN_KEY` reads that from the column's
    // flag rather than from the indexes. Measured on 8.3: `UNIQUE uq (a, b)`
    // with `b` dropped reports `a` as `UNI`, where the index alone left it
    // `MUL`. The primary-key side needs nothing — a composite primary key
    // flags every one of its columns already.
    let now_unique: Vec<String> = table
        .indexes
        .iter()
        .filter(|i| i.unique && !i.primary && i.columns.len() == 1)
        .map(|i| i.columns[0].clone())
        .collect();
    for name in now_unique {
        if let Some(at) = table.column_index(&name) {
            table.columns[at].unique = true;
        }
    }
}

/// Carries a renamed column into the keys that name it.
///
/// A key holds its columns by name, so a rename that touched only
/// `table.columns` left `information_schema` reporting an index and a foreign
/// key over a name the table no longer has — the same phantom
/// [`narrow_indexes`] exists to prevent, through a different door, and it also
/// hid the renamed column from the 1828 rule, which then let a foreign key's
/// own column be dropped under its new name. 8.3 carries both: after
/// `RENAME COLUMN pid TO qid`, `STATISTICS` and `KEY_COLUMN_USAGE` both say
/// `qid` there, and `DROP COLUMN qid` afterwards is 1828.
///
/// The referencing side only. The columns a key in another table *points at*
/// are carried by [`rename_referenced_columns`], which runs once at the end
/// because it writes outside the table being altered.
fn rename_key_column(table: &mut Table, old: &str, new: &str) {
    for name in table
        .indexes
        .iter_mut()
        .flat_map(|i| i.columns.iter_mut())
        .chain(
            table
                .foreign_keys
                .iter_mut()
                .flat_map(|f| f.columns.iter_mut()),
        )
    {
        if name.eq_ignore_ascii_case(old) {
            *name = new.to_owned();
        }
    }
}

/// The names a column list ends up with after a statement's renames.
///
/// In the statement's order, so a column renamed twice arrives at the second
/// name rather than at the first.
fn after_renames(columns: &[String], renames: &[(String, String)]) -> Vec<String> {
    columns
        .iter()
        .map(|column| {
            let mut name = column.clone();
            for (old, new) in renames {
                if name.eq_ignore_ascii_case(old) {
                    name = new.clone();
                }
            }
            name
        })
        .collect()
}

/// Carries this table's column renames into the foreign keys that point at it.
///
/// The other half of [`rename_key_column`], and it has to be a separate pass
/// because it writes to *other* tables: 8.3 renames a parent's column in every
/// child's constraint too, and `KEY_COLUMN_USAGE.REFERENCED_COLUMN_NAME` says
/// the new name there afterwards. Left behind, the constraint points at a
/// column that is not there, which stops it being enforced at all — the
/// referenced positions no longer resolve, and an unresolvable key is skipped.
///
/// Every database and not only the altered table's own, for the same reason
/// [`rename_referencing_tables`] crosses them: measured on 8.3,
/// `ALTER TABLE m3.PP RENAME COLUMN u TO w` leaves a key on `m4.CC` written
/// `REFERENCES m3.PP(u)` reporting `w`. Walking one database renamed half of
/// such a child — the table name followed and the column did not, so the
/// constraint named the right parent and a column that had gone.
///
/// Returns the tables it changed, by database, which a transaction has to know
/// about for the same reason the altered table itself is marked.
fn rename_referenced_columns(
    catalog: &mut Catalog,
    database: &str,
    table_name: &str,
    renames: &[(String, String)],
) -> Vec<(String, String)> {
    walk_referencing_keys(catalog, database, table_name, |fk| {
        let renamed = after_renames(&fk.referenced_columns, renames);
        if renamed == fk.referenced_columns {
            return false;
        }
        fk.referenced_columns = renamed;
        true
    })
}

/// Every foreign key in the server that points at `database`.`table_name`,
/// handed to `change` — which says whether it changed anything.
///
/// Shared by the column half and the table half of the same propagation, so
/// there is one answer to "which keys point here" rather than two that have to
/// agree. Returns the tables `change` reported a change on, by database.
fn walk_referencing_keys(
    catalog: &mut Catalog,
    database: &str,
    table_name: &str,
    mut change: impl FnMut(&mut ForeignKey) -> bool,
) -> Vec<(String, String)> {
    let mut touched = Vec::new();
    for db_name in catalog.database_names() {
        let Some(db) = catalog.database_mut(&db_name) else {
            continue;
        };
        let own = db.name.clone();
        for other in db.tables.values_mut() {
            let mut changed = false;
            for fk in &mut other.foreign_keys {
                if fk.points_at(&own, database, table_name) && change(fk) {
                    changed = true;
                }
            }
            if changed {
                touched.push((own.clone(), other.name.clone()));
            }
        }
    }
    touched
}

/// Carries a table's *own* rename into the names of the unnamed foreign keys
/// it holds.
///
/// Measured on 8.3, and it is not the propagation the rest of this is about —
/// this one is the child's side. `ALTER TABLE midt RENAME TO midt2` leaves
/// `midt_ibfk_1` reported as `midt2_ibfk_1`, and it is a *textual* rule rather
/// than one about which keys MySQL named itself: a constraint written
/// `CONSTRAINT named_ibfk_9` on a table `named` becomes `named2_ibfk_9` when
/// the table becomes `named2`, while `other_ibfk_1` on a table `odd` renamed
/// to `odd2` is left exactly as it is. So the prefix decides, and nothing
/// about how the name came to be.
///
/// **The one identifier comparison here that is case-sensitive**, and that is
/// measured rather than assumed — it was written the other way first. On 8.3
/// `CONSTRAINT T_IBFK_1` on a table `T` renamed to `T2` stays `T_IBFK_1`, and
/// so does `tc_ibfk_5` on a table `Tc` renamed to `Tc2`. Both halves of the
/// prefix are compared byte for byte, which is what a name built by
/// concatenation rather than looked up in a catalog gets.
///
/// The *index* behind the key is not renamed with it, also measured: the
/// `named_ibfk_9` index is still called that on `named2` afterwards.
fn rename_own_auto_keys(table: &mut Table, old_name: &str, new_name: &str) {
    for fk in &mut table.foreign_keys {
        fk.name = renamed_auto_key(&fk.name, old_name, new_name);
    }
    // Checks too, and by the same rule. Measured on 8.3: `T` holding
    // `T_chk_1` renamed to `T2` holds `T2_chk_1`, while a check written as
    // `named` keeps its name. Left behind, the generated one went on claiming
    // the old table's name — so recreating a table under that name was a
    // spurious 3822, which is drop-and-recreate under migrations and exactly
    // what this engine is for.
    for check in &mut table.checks {
        check.name = renamed_auto_check(&check.name, old_name, new_name);
    }
}

/// What a rename does to one check's name.
///
/// The `_chk_` counterpart of [`renamed_auto_key`], and the same shape: a name
/// that does not start with the old table's prefix is one the statement chose,
/// and is left alone.
fn renamed_auto_check(name: &str, old_name: &str, new_name: &str) -> String {
    match name.strip_prefix(&format!("{old_name}_chk_")) {
        Some(tail) if !tail.is_empty() => format!("{new_name}_chk_{tail}"),
        _ => name.to_owned(),
    }
}

/// What a rename does to one key's name, on its own.
///
/// Split out so that `RENAME TABLE`'s dry run and the write that follows it
/// cannot come to different conclusions — the dry run has to know the names
/// the statement will produce in order to refuse a collision before anything
/// is written.
///
/// Something has to follow the prefix. Every name measured on 8.3 carried a
/// number after it, and a name that is the prefix and nothing else was not, so
/// it is left alone rather than guessed at.
fn renamed_auto_key(name: &str, old_name: &str, new_name: &str) -> String {
    match name.strip_prefix(&format!("{old_name}{IBFK}")) {
        Some(tail) if !tail.is_empty() => format!("{new_name}{IBFK}{tail}"),
        _ => name.to_owned(),
    }
}

/// Carries a table's rename into every foreign key that points at it.
///
/// The table half of what `rename_referenced_columns` does for columns, and
/// #0205: left behind, a child's `REFERENCED_TABLE_NAME` names a table that no
/// longer exists, so the constraint is both wrong in `information_schema` and
/// unenforceable — `crate::foreign_keys::snapshot` finds no parent and the key
/// goes inert.
///
/// Every database and not only the renamed table's own, because a child
/// elsewhere is a child: measured on 8.3, renaming `probe.p` to `p2` rewrites
/// the key on `other.oc` that was written `REFERENCES probe.p(id)`. The
/// database the constraint named is untouched — only the parent's own name
/// moves — so a key that named its parent's database goes on naming it.
///
/// Returns the tables it changed, by database, which a transaction has to know
/// about for the same reason the renamed table itself is marked.
fn rename_referencing_tables(
    catalog: &mut Catalog,
    database: &str,
    old_name: &str,
    new_name: &str,
) -> Vec<(String, String)> {
    walk_referencing_keys(catalog, database, old_name, |fk| {
        fk.referenced_table = new_name.to_owned();
        true
    })
}

/// Whether a foreign key on a table in `database` points at `table_name`,
/// which is in that same database.
///
/// The database is compared and not just the name, because a key written
/// `REFERENCES other.P` from a child in `app` names a different parent than a
/// bare `REFERENCES P` does, and the rules below are about `app.P`. Compared
/// against the joined `other.P`, every qualified constraint fell outside the
/// rules below and its parent's column could be dropped from under it.
fn references_table(fk: &ForeignKey, database: &str, table_name: &str) -> bool {
    fk.points_at(database, database, table_name)
}

/// Whether an index can answer a lookup on exactly `columns`.
///
/// A *leading* prefix, in order, which is the rule MySQL binds a foreign key
/// to an index by: measured on 8.3, `KEY wide (pid, x)` serves
/// `FOREIGN KEY (pid)` — no index of the constraint's own is created beside it
/// — while `KEY tail (x, pid)` does not and one is. Uniqueness is no part of
/// it on either side of the constraint: a parent carrying `UNIQUE uq (u)`
/// beside `KEY spare (u)` accepts `DROP INDEX uq` there and refuses
/// `DROP INDEX spare` afterwards.
pub(crate) fn index_serves(index: &Index, columns: &[String]) -> bool {
    columns.len() <= index.columns.len()
        && columns
            .iter()
            .zip(&index.columns)
            .all(|(wanted, held)| wanted.eq_ignore_ascii_case(held))
}

/// Whether any of the table's indexes serves `columns`.
fn served_by_some_index(table: &Table, columns: &[String]) -> bool {
    table.indexes.iter().any(|i| index_serves(i, columns))
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

/// Refuses a constraint name the database already holds.
///
/// A foreign key's name and a `CHECK`'s are scoped to the *database*, where an
/// index name is scoped to its table. This engine enforced neither, so
/// `ALTER TABLE A ADD CONSTRAINT k FOREIGN KEY ...` followed by the same
/// against `B` gave one database two constraints called `k` — and then
/// reported both from `information_schema`, where Prisma's migration engine
/// reads them back and compares them against the schema. #0257.
///
/// Checked against the table as it will be, not as it was, and called once the
/// table is otherwise built. Two reasons, both measured:
///
/// * 1061 wins where it applies. A foreign key makes an index of its own name,
///   so `ADD CONSTRAINT k` on a table already holding an index `k` is the
///   index's answer, not this one — and running this first would report 1826
///   where 8.3 reports 1061. Adding the key to a table whose index name is
///   *free* (renamed away, or covered by another index so no index is made) is
///   1826, which is what says this rule is real rather than the index rule
///   wearing a different number.
/// * `ALTER TABLE t DROP CHECK c, ADD CONSTRAINT c CHECK (...)` is accepted,
///   so the name has to be judged on what the statement leaves behind.
///
/// The two namespaces are separate: a `CHECK` may take a name a foreign key
/// holds, and the other way round, both measured.
fn refuse_duplicate_constraint_names(
    database: &Database,
    replacing: &str,
    table: &Table,
) -> EngineResult<()> {
    // Every other table's names. The table being written is excluded and its
    // own names come from `table`, because that is the version this statement
    // produces and the stored one is about to be replaced by it.
    let mut keys: Vec<&str> = Vec::new();
    let mut checks: Vec<&str> = Vec::new();
    for (name, other) in &database.tables {
        if name.eq_ignore_ascii_case(replacing) {
            continue;
        }
        keys.extend(other.foreign_keys.iter().map(|f| f.name.as_str()));
        checks.extend(other.checks.iter().map(|c| c.name.as_str()));
    }
    // Spelled as this statement wrote it, not as the holder did: 8.3 reports
    // `K` for a `K` added after a `k`.
    for key in &table.foreign_keys {
        if keys.iter().any(|held| held.eq_ignore_ascii_case(&key.name)) {
            return Err(EngineError::duplicate_foreign_key_name(&key.name));
        }
        keys.push(&key.name);
    }
    for check in &table.checks {
        if checks
            .iter()
            .any(|held| held.eq_ignore_ascii_case(&check.name))
        {
            return Err(EngineError::duplicate_check_name(&check.name));
        }
        checks.push(&check.name);
    }
    Ok(())
}

/// Adds an index, refusing a name the table already carries.
///
/// An index name is scoped to its table, so the same name on two tables is
/// legal and nothing outside `table` is consulted. Measured on 8.3, where 1061
/// is the answer for a duplicate inside one CREATE TABLE, for a second CREATE
/// INDEX, and for ALTER TABLE ADD INDEX and ADD CONSTRAINT alike.
///
/// The exception is an index a foreign key made for itself, which 8.3 treats as
/// provisional: an explicit index that can serve the key takes over, and the
/// provisional one — and the name it was holding — goes. That is not a detail.
/// Prisma emits the pair
///
/// ```sql
/// ALTER TABLE `Job` ADD CONSTRAINT `Job_rateTypeId_fkey` FOREIGN KEY ...;
/// CREATE INDEX `Job_rateTypeId_fkey` ON `Job` (`rateTypeId`);
/// ```
///
/// and refusing the second statement stopped the real migration history dead at
/// its twenty-third migration. #0255.
///
/// Measured on 8.3, and the whole table is needed to get it right:
///
/// | after a foreign key `c` on `(r)` | |
/// |---|---|
/// | `CREATE INDEX c ON J(r)` | accepted, one index `c` on `r` |
/// | `CREATE INDEX c ON J(s)` | 1061 — cannot serve the key, so `c` is still taken |
/// | `CREATE INDEX other ON J(r)` | accepted, and provisional `c` is *gone* |
/// | the same `CREATE INDEX c ON J(r)` twice | the second is 1061 |
/// | an *explicit* `c` on `(r)` the key later reused | 1061 |
///
/// The last two rows are why `Index::implicit_for_foreign_key` is remembered
/// rather than worked out: after the takeover the index is an ordinary one, and
/// an explicit index is an ordinary one from the start, and neither is
/// distinguishable from the provisional case by name and columns alone.
fn add_index(table: &mut Table, index: Index) -> EngineResult<()> {
    // Worked out before anything is removed, and removed only once every
    // refusal is behind us. `create_index` holds a borrow of the *live*
    // catalog rather than a clone the way `alter_table` does, so a takeover
    // applied ahead of a refusal is not rolled back: measured, superseding
    // first left a refused `CREATE INDEX` having destroyed the index its
    // foreign key was relying on, where 8.3 leaves the table untouched.
    let superseded = provisional_indexes_superseded_by(table, &index);
    if !index.primary && index_name_taken_apart_from(table, &index.name, &superseded) {
        return Err(EngineError::duplicate_key_name(&index.name));
    }
    drop_superseded(table, &superseded);
    table.indexes.push(index);
    Ok(())
}

/// The provisional indexes that `index` would take over, named.
///
/// Worked out without touching the table, so a caller can decide whether a name
/// is really taken before committing to anything — a name held by an index
/// about to be taken over is exactly the case that must *not* be 1061.
///
/// **Column coverage alone, and no foreign key is consulted.** That is measured
/// and was got wrong first: an index MySQL made for a key is replaceable
/// wherever another index covers it, whether or not the key is still there.
/// `ADD CONSTRAINT k FOREIGN KEY (r)`, `DROP FOREIGN KEY k`, `CREATE INDEX z ON
/// T (r)` leaves 8.3 holding `z` alone; the same with `z` over a different
/// column leaves it holding both, which is what says the rule is coverage.
///
/// Nothing about the incoming index is excluded either, and both exclusions
/// that suggested themselves were measured and are wrong: a **primary key**
/// takes over (`ADD PRIMARY KEY (r)` over a key on `(r)` leaves `PRIMARY`
/// alone), and so does **another key's** provisional index (`c1 (r)` then
/// `c2 (r, b)` leaves `c2` alone, because `(r, b)` covers `(r)`).
fn provisional_indexes_superseded_by(table: &Table, index: &Index) -> Vec<String> {
    table
        .indexes
        .iter()
        .filter(|held| held.implicit_for_foreign_key && index_serves(index, &held.columns))
        .map(|held| held.name.clone())
        .collect()
}

/// Whether the table holds an index called `name` other than one being taken
/// over.
/// `index_name_taken`, less the indexes about to be taken over.
///
/// The `!primary` filter is carried across deliberately and not by accident:
/// `index_name_taken`'s own doc records that a primary key is left out because
/// 8.3 answers 1280 for `` CREATE INDEX `PRIMARY` ``, and folding it in here
/// would trade an accepted statement for a wrongly numbered refusal. Dropping
/// it silently is what the first version of this function did.
fn index_name_taken_apart_from(table: &Table, name: &str, superseded: &[String]) -> bool {
    table.indexes.iter().any(|held| {
        !held.primary && held.name.eq_ignore_ascii_case(name) && !is_superseded(held, superseded)
    })
}

fn is_superseded(held: &Index, superseded: &[String]) -> bool {
    held.implicit_for_foreign_key && superseded.iter().any(|n| held.name.eq_ignore_ascii_case(n))
}

fn drop_superseded(table: &mut Table, superseded: &[String]) {
    table
        .indexes
        .retain(|held| !is_superseded(held, superseded));
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
                    implicit_for_foreign_key: false,
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
                    implicit_for_foreign_key: false,
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
                    implicit_for_foreign_key: false,
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
                .unwrap_or_else(|| format!("{}{IBFK}{}", table.name, table.foreign_keys.len() + 1));
            let key_columns: Vec<String> = columns.iter().map(|c| c.value.clone()).collect();

            // MySQL creates an index for a foreign key unless one already
            // covers its columns, and names it after the constraint. Prisma
            // then renames that index in a later migration, which has nothing
            // to find if the index was never created.
            // Through the same predicate the drop refusals read, so there is
            // one answer to "which index backs this key" rather than two that
            // have to agree — and this one compares the names the way every
            // other identifier comparison here does, where `starts_with` on
            // the raw strings made `KEY IX (PID)` not cover `(pid)`.
            let covered = served_by_some_index(table, &key_columns);
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
                        implicit_for_foreign_key: true,
                    },
                )?;
            }

            // The parent kept as the two parts it was written as, never joined
            // with a dot: `name::dotted` says in as many words that its join
            // is only for a name being *matched* against another built the
            // same way, because ``REFERENCES `app.P` `` and
            // `REFERENCES app.P` render identically and a split cannot tell
            // them apart afterwards. Both halves are reported separately by
            // `information_schema`, so both have to survive.
            let mut parts = name::parts(foreign_table);
            let referenced_table = parts.pop().unwrap_or_default();
            let referenced_schema = parts.pop();

            table.foreign_keys.push(ForeignKey {
                name: constraint_name,
                columns: key_columns,
                referenced_schema,
                referenced_table,
                referenced_columns: referred_columns.iter().map(|c| c.value.clone()).collect(),
                // `NO ACTION` and not `RESTRICT` for an action the constraint
                // did not write, which is #0206: 8.3 reports `NO ACTION` in
                // `REFERENTIAL_CONSTRAINTS` there and keeps `RESTRICT` for the
                // one written that way, so the two spellings are told apart in
                // what a client reads even though InnoDB enforces them alike.
                // `foreign_keys::Action::parse` maps both onto `Restrict`, so
                // nothing about enforcement moves with this.
                //
                // The two are independent: `ON DELETE CASCADE` alone reports
                // `CASCADE` beside an `UPDATE_RULE` of `NO ACTION`, measured
                // both ways round.
                on_delete: on_delete
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| NO_ACTION.into()),
                on_update: on_update
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| NO_ACTION.into()),
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

/// `RENAME TABLE a TO b, c TO d`.
///
/// A statement of its own rather than a spelling of `ALTER TABLE ... RENAME
/// TO`, because it renames *several* tables and either all of them happen or
/// none do. Measured on 8.3: `RENAME TABLE A TO A2, B TO B2` moves both, and a
/// pair that fails leaves every earlier pair alone -- so the whole list is
/// checked before any of it is written, the way `alter_table` checks its
/// operations.
///
/// The two refusals are measured too, and neither is the one the engine gave
/// before (which was 1235 for the statement itself):
///
/// ```text
/// RENAME TABLE A2 TO Keep   -- Keep exists      1050 Table 'Keep' already exists
/// RENAME TABLE nosuch TO x                      1146 Table 'probe.nosuch' doesn't exist
/// ```
///
/// Note 1146 names the table **qualified** here, where the same error from a
/// SELECT does not. That is 8.3's spelling, not a slip.
///
/// A target in another database *moves* the table there on 8.3. That is
/// refused rather than implemented, because a move has to carry the rows, the
/// keys and every foreign key pointing at the table across two databases at
/// once, and doing half of it is how a table ends up somewhere nothing can
/// address. See #0251.
pub fn rename_tables(
    engine: &Engine,
    session: &mut Session,
    pairs: &[sqlparser::ast::RenameTable],
) -> EngineResult<Outcome> {
    // Resolved before anything is written, so a bad pair late in the list
    // cannot leave an earlier one applied.
    let mut planned: Vec<(String, String, String)> = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let (db, from) = engine.split_name_public(session, &pair.old_name)?;
        let (target_db, to) = engine.split_name_public(session, &pair.new_name)?;
        if !target_db.eq_ignore_ascii_case(&db) {
            return Err(crate::error::unsupported(format!(
                "moving `{from}` to another database with RENAME TABLE"
            )));
        }
        planned.push((db, from, to));
    }

    {
        let catalog = engine.catalog_public()?;
        // Every pair against the catalog as it stands, plus the targets this
        // statement is itself about to create: `RENAME TABLE A TO B, B TO C`
        // is legal on 8.3 because B is gone by the time the second pair runs,
        // while `A TO X, B TO X` is not.
        // A running picture of what exists, not a set of exceptions to the
        // catalog: 8.3 applies the pairs *in order*, so `A TO Tmp, Tmp TO A3`
        // is legal -- Tmp exists by the time the second pair reads it -- and
        // `A TO X, B TO X` is not. Only a simulation gets both.
        let mut names: Vec<(String, String)> = Vec::new();
        let mut seeded: Vec<String> = Vec::new();
        // Alongside the table names, the constraint names each table carries,
        // because a rename rewrites the keys named after the table and can
        // therefore put two of one name in a database. Simulated rather than
        // checked afterwards for the reason the table names are: this
        // statement is all-or-nothing, and by the time the write loop below
        // runs the earlier pairs have been applied. Measured on 8.3, which
        // answers 1826 and leaves every table where it was.
        let mut held: Vec<(String, String, String)> = Vec::new();
        // Checks in a pool of their own, because the two namespaces are
        // independent: measured, a rename onto a check name that only a
        // *foreign key* holds is accepted, and the refusal when it does
        // collide is 3822 rather than 1826. Folding them together would refuse
        // statements 8.3 answers.
        let mut held_checks: Vec<(String, String, String)> = Vec::new();
        for (db, from, to) in &planned {
            let database = catalog
                .database(db)
                .ok_or_else(|| EngineError::unknown_database(db))?;
            if !seeded.iter().any(|d| d == db) {
                names.extend(database.tables.keys().map(|k| (db.clone(), k.clone())));
                for (owner, table) in &database.tables {
                    held.extend(
                        table
                            .foreign_keys
                            .iter()
                            .map(|f| (db.clone(), owner.clone(), f.name.clone())),
                    );
                    held_checks.extend(
                        table
                            .checks
                            .iter()
                            .map(|c| (db.clone(), owner.clone(), c.name.clone())),
                    );
                }
                seeded.push(db.clone());
            }
            // The target before the source, which is measurable only when
            // both are wrong at once: `RENAME TABLE gone TO Have` with `gone`
            // missing and `Have` present is 1050 on 8.3, not 1146.
            //
            // And with no exemption for a rename onto the same name --
            // `RENAME TABLE Have2 TO Have2` is 1050 there too, where the
            // `ALTER TABLE ... RENAME TO` spelling treats it as the no-op it
            // looks like. Two statements, two answers; this is the one that
            // refuses.
            if names
                .iter()
                .any(|(d, t)| d == db && t.eq_ignore_ascii_case(to))
            {
                return Err(EngineError::table_exists(to));
            }
            let at = names
                .iter()
                .position(|(d, t)| d == db && t.eq_ignore_ascii_case(from));
            let Some(at) = at else {
                return Err(EngineError::no_such_table(&format!("{db}.{from}")));
            };
            // The name as the catalog *stores* it, not as this statement
            // typed it. `renamed_auto_key` strips a byte-exact prefix, so a
            // dry run driven off the typed spelling reaches a different
            // conclusion from the write, which is driven off `table.name`:
            // `RENAME TABLE a TO b` over a stored `A` left the dry run seeing
            // no collision and the write producing `b_ibfk_1` beside another
            // table's. Measured.
            let stored = names[at].1.clone();
            names.remove(at);
            names.push((db.clone(), to.clone()));

            // The renamed table's own keys follow it, and the result must not
            // be a name the database already holds. Applied pair by pair,
            // because that is the order 8.3 applies them in and the pair that
            // collides is the one it names.
            for entry in &mut held {
                if entry.0 == *db && entry.1.eq_ignore_ascii_case(&stored) {
                    entry.1 = to.clone();
                    entry.2 = renamed_auto_key(&entry.2, &stored, to);
                }
            }
            // The checks named after the table follow it too, by the rule
            // `rename_own_auto_keys` applies when the write happens. Simulated
            // here for the same reason the keys are: without it a rename that
            // collides two check names was accepted, and the duplicate it
            // created then refused every later `ALTER TABLE` on *both* tables,
            // which only dropping one could clear.
            for entry in &mut held_checks {
                if entry.0 == *db && entry.1.eq_ignore_ascii_case(&stored) {
                    entry.1 = to.clone();
                    entry.2 = renamed_auto_check(&entry.2, &stored, to);
                }
            }
            for at in 0..held.len() {
                let (edb, _, name) = &held[at];
                if held[..at]
                    .iter()
                    .any(|(d, _, earlier)| d == edb && earlier.eq_ignore_ascii_case(name))
                {
                    return Err(EngineError::duplicate_foreign_key_name(name));
                }
            }
            for at in 0..held_checks.len() {
                let (edb, _, name) = &held_checks[at];
                if held_checks[..at]
                    .iter()
                    .any(|(d, _, earlier)| d == edb && earlier.eq_ignore_ascii_case(name))
                {
                    return Err(EngineError::duplicate_check_name(name));
                }
            }
        }
    }

    for (db, from, to) in &planned {
        engine.mark_modified_public(session, db, from);
        engine.mark_modified_public(session, db, to);
        let mut catalog = engine.catalog_mut_public()?;
        // The foreign keys that point at the old name, before the table moves
        // out from under them -- the same propagation `ALTER TABLE ... RENAME
        // TO` does, and the reason a renamed parent does not leave its
        // children pointing at a table that is gone.
        let touched = rename_referencing_tables(&mut catalog, db, from, to);
        let database = catalog
            .database_mut(db)
            .ok_or_else(|| EngineError::unknown_database(db))?;
        let key = database
            .tables
            .keys()
            .find(|k| k.eq_ignore_ascii_case(from))
            .cloned();
        if let Some(key) = key {
            let Some(mut table) = database.tables.remove(&key) else {
                continue;
            };
            let was = table.name.clone();
            rename_own_auto_keys(&mut table, &was, to);
            table.name = to.clone();
            database.tables.insert(to.clone(), table);
        }
        drop(catalog);
        for (other_db, other_table) in touched {
            engine.mark_modified_public(session, &other_db, &other_table);
        }
    }

    Ok(Outcome::ok())
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

    // The indexes as the statement found them, kept because 1553 below names
    // the index that was *dropped* and by then it is gone from the copy.
    let original_indexes = table.indexes.clone();

    // The columns this statement takes away, and whether the loop refuses it
    // first for a reason the rule below must not get in front of.
    let walk = walk_columns(table, operations);
    let dropped_columns = walk.dropped;

    // The referencing half of the foreign key rule, and it sits here rather
    // than after the loop because 8.3 settles it there: `DROP COLUMN pid,
    // DROP INDEX nosuch` and the same pair reversed are both 1828 on a child
    // table, so the rule outranks an operation that does not resolve. It
    // outranks a duplicate column name (1060) too, and a `DROP COLUMN` of a
    // name the table has not got (1091), both measured both ways round.
    //
    // Two refusals still beat it, and each is skipped rather than reordered so
    // that nothing else about the loop moves — see `ColumnWalk::answered_first`.
    //
    // Against the foreign keys the statement *leaves*, which is what makes
    // `DROP COLUMN pid, DROP FOREIGN KEY fk7` legal on 8.3 in either order —
    // and it is how a migration retires a column and its constraint at once.
    if !dropped_columns.is_empty() && !walk.answered_first {
        for fk in surviving_foreign_keys(table, operations) {
            for column in &fk.columns {
                if dropped_columns
                    .iter()
                    .any(|d| d.eq_ignore_ascii_case(column))
                {
                    return Err(EngineError::fk_column_cannot_drop(column, &fk.name));
                }
            }
        }
    }

    // Recorded rather than applied, which is the whole of #0184: this arm used
    // to write to the catalog and return, so every operation written after the
    // rename was skipped in silence and `RENAME TO t2, DROP INDEX nosuch`
    // renamed the table where 8.3 answers 1091 and leaves it alone. Hoisted
    // out, the statement is settled before any of it applies, like every other
    // operation here. The last one wins when a statement writes two, as it
    // does on 8.3: `RENAME TO r7b, RENAME TO r7c` leaves `r7c`.
    let mut renamed_to: Option<String> = None;

    // The column renames the statement makes, in the order it makes them.
    // Recorded because a key in *another* table may point at one of them, and
    // that table cannot be written to until the whole statement has passed.
    let mut renamed_columns: Vec<(String, String)> = Vec::new();

    // The name a rename left the table under, for the transaction to mark.
    let mut moved_to: Option<String> = None;

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
                    return Err(EngineError::duplicate_column(&column_def.name.value));
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
                            let removed = table.columns.remove(index).name;
                            for row in &mut table.rows {
                                if index < row.len() {
                                    row.remove(index);
                                }
                            }
                            for (position, column) in table.columns.iter_mut().enumerate() {
                                column.position = position + 1;
                            }
                            // The column leaves its keys with it, which is
                            // #0179: leaving them behind listed an index over
                            // a column that is gone in
                            // `information_schema.STATISTICS`, and since #0150
                            // that phantom also holds its name against a later
                            // `CREATE INDEX` that MySQL accepts.
                            //
                            // By the stored spelling rather than the one the
                            // statement wrote, because that is what the
                            // indexes hold.
                            narrow_indexes(table, &removed);
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
                // Renaming onto a name the table already carries would leave
                // two columns of one name, which every reference then resolves
                // to the first of — and the keys, which hold their columns by
                // name, could no longer say which they were over. The column's
                // own name is not that, and `RENAME COLUMN a TO A` succeeds on
                // 8.3 as it does here.
                if matches!(table.column_index(&new_column_name.value), Some(other) if other != index)
                {
                    return Err(EngineError::duplicate_column(&new_column_name.value));
                }
                let old = std::mem::replace(
                    &mut table.columns[index].name,
                    new_column_name.value.clone(),
                );
                rename_key_column(table, &old, &new_column_name.value);
                renamed_columns.push((old, new_column_name.value.clone()));
            }
            AlterTableOperation::RenameTable {
                table_name: new_name,
            } => {
                // The kind's Display renders "TO Gadget"; the name is inside
                // it, and using the rendered form names the table "TO Gadget".
                //
                // Noted and not applied — see `renamed_to` above. The move
                // itself is the write-back at the end of this function, which
                // is the only place the catalog is touched now.
                let target = match new_name {
                    RenameTableNameKind::To(name) | RenameTableNameKind::As(name) => name,
                };
                // A qualified target names another database, and 8.3 *moves*
                // the table there. Joining the parts back into one name --
                // which is what this did -- produced a table literally called
                // `other.Alt` in the current database, which no qualified name
                // then reaches: `SELECT i FROM other.Alt` is 1146 while the
                // rows sit in the catalog under a name nothing can address.
                //
                // Refused rather than half-implemented. A move has to carry
                // the rows, the keys and every foreign key pointing at the
                // table across two databases at once, and stranding a table is
                // a worse answer than declining to move it. See #0251.
                let mut parts = name::parts(target);
                if parts.len() > 1 && !parts[0].eq_ignore_ascii_case(&db) {
                    return Err(crate::error::unsupported(format!(
                        "moving `{table_name}` to database `{}`",
                        parts[0]
                    )));
                }
                let bare = parts.pop().unwrap_or_default();
                renamed_to = Some(bare);
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
                // `CHANGE COLUMN` renames, so it collides the same way
                // `RENAME COLUMN` does above.
                if matches!(table.column_index(&new_name.value), Some(other) if other != index) {
                    return Err(EngineError::duplicate_column(&new_name.value));
                }
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
                // `CHANGE COLUMN` renames as well as retypes, and the keys
                // over the old name have to follow it for the same reason
                // `RENAME COLUMN`'s do.
                rename_key_column(table, &old_name.value, &new_name.value);
                renamed_columns.push((old_name.value.clone(), new_name.value.clone()));
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

    // Four rules the statement is judged by as a whole rather than operation
    // by operation, in the order 8.3 answers them — each pairing was measured
    // both ways round, because an ALTER that breaks two rules reports one.
    //
    // All four sit *after* the loop, so an operation that does not resolve
    // wins over every one of them: `ADD INDEX ixz (nosuch), DROP INDEX
    // nosuchidx` is 1091 there and not the 1072 the first half would earn.
    // The referencing half of the foreign key rule is the exception and is
    // handled before the loop; the note there says why.

    // 1072: a key the statement adds is resolved against the table the
    // statement *leaves*, so `DROP COLUMN a, ADD INDEX ixa (a)` and
    // `ADD INDEX ixa (a), DROP COLUMN a` are both refused on 8.3 — the second
    // one despite the add having succeeded against the table as it stood.
    //
    // Read off the operations rather than off `table.indexes`, and that is the
    // half the reversed order needs: narrowing takes the dropped column out of
    // the index the add had just made, which leaves it with no columns and so
    // removes it, and a scan of what survived would find nothing to complain
    // about.
    for operation in operations {
        let AlterTableOperation::AddConstraint { constraint, .. } = operation else {
            continue;
        };
        for column in key_columns_declared(constraint) {
            if table.column_index(&column).is_none() {
                return Err(EngineError::key_column_missing(&column));
            }
        }
    }

    // Every foreign key that points at this table, its own included: a
    // self-referencing key is on both sides at once and 8.3 answers it with
    // the *referenced* side's code, naming its own table. This table's keys
    // are read from the copy so that one the statement dropped no longer
    // counts; the other tables' from the catalog, since the statement cannot
    // have touched them.
    let referencing = keys_pointing_at(database, &table_name, table);

    // 1829: the referenced half of the foreign key rule, 1828 above being the
    // referencing half.
    //
    // Of the columns this statement dropped and did not put back. A name that
    // is gone from the copy but was never named by a `DROP COLUMN` was renamed
    // rather than dropped, and a rename is not what this refuses.
    for dropped in &dropped_columns {
        if table.column_index(dropped).is_some() {
            continue;
        }
        for (other, fk) in &referencing {
            if fk
                .referenced_columns
                .iter()
                .any(|c| c.eq_ignore_ascii_case(dropped))
            {
                return Err(EngineError::fk_column_cannot_drop_child(
                    dropped, &fk.name, other,
                ));
            }
        }
    }

    // 1553, through the same helper the standalone `DROP INDEX ... ON t` calls.
    refuse_unbacked_foreign_key(
        database,
        &table_name,
        table,
        &original_indexes,
        &renamed_columns,
    )?;

    // 1062: a unique key narrowed onto columns whose rows now collide. The
    // narrowing above is what makes this reachable — an index over a column
    // that had been dropped was inert, `check_unique` skipping any key whose
    // columns do not resolve, and a narrowed one is live again. 8.3 refuses
    // the whole statement: `UNIQUE (a, b)` over `(1, 1), (1, 2)` with `b`
    // dropped is `Duplicate entry '1'` there and leaves the table alone.
    //
    // Only when a key actually narrowed, because this is quadratic in the rows
    // and every other ALTER would pay for it.
    if table
        .indexes
        .iter()
        .filter(|i| i.unique || i.primary)
        .any(|i| {
            original_indexes
                .iter()
                .any(|o| o.name.eq_ignore_ascii_case(&i.name) && o.columns.len() > i.columns.len())
        })
    {
        for (position, row) in table.rows.iter().enumerate() {
            table.check_unique(row, Some(position))?;
        }
    }

    // 1050: a rename onto a name the database already holds. `insert` would
    // otherwise replace that table — its columns and its rows — with this one
    // and report success, which is the only place in this function that
    // destroys data nothing asked it to. 8.3 refuses and leaves both alone.
    //
    // A rename onto the table's *own* name is not that: it differs only in
    // case, or not at all, and removing the old key before inserting makes it
    // the no-op it should be.
    if let Some(new_name) = &renamed_to {
        if !new_name.eq_ignore_ascii_case(&table_name) && database.table(new_name).is_some() {
            return Err(EngineError::table_exists(new_name));
        }
    }

    // Against the table as this statement leaves it, and against the rest of
    // the database, which is the scope a constraint name is unique in. Last,
    // so that every refusal an operation raises for itself — 1061 among them,
    // which beats this one — has already been raised. #0257.
    // The keys named after the table follow it, and they have to follow it
    // *before* the names are judged: `RENAME TO B2` turns `A_ibfk_1` into
    // `B2_ibfk_1`, which another table may already hold. Measured on 8.3,
    // which answers 1826 and leaves the table where it was. Applied to the
    // copy, so a refusal below still leaves the stored table untouched.
    if let Some(new_name) = &renamed_to {
        let was = table.name.clone();
        rename_own_auto_keys(table, &was, new_name);
    }

    refuse_duplicate_constraint_names(database, &table_name, table)?;

    // Every operation succeeded, so the copy becomes the table. Written back
    // under the stored key rather than what the user typed, which can differ
    // in case — inserting under the typed spelling would leave the rows under
    // both names.
    let key = database
        .tables
        .keys()
        .find(|k| k.eq_ignore_ascii_case(&table_name))
        .cloned();
    if let Some(key) = key {
        resolve_collations(table, &database.collation);
        match renamed_to {
            // Removed by the stored key and inserted under the new name, so a
            // rename that differs only in case does not leave the rows under
            // both.
            Some(new_name) => {
                database.tables.remove(&key);
                // The keys themselves were renamed above, before the names
                // were judged; only the table's own name is left to set.
                table.name = new_name.clone();
                database.tables.insert(new_name.clone(), table.clone());
                // The name the table arrives under is marked too, and not only
                // the one it left: a rollback restores what the snapshot held
                // for every table the transaction touched and *removes* the
                // ones it does not know about, so an unmarked target survived
                // the rollback beside the original coming back.
                moved_to = Some(new_name);
            }
            None => {
                database.tables.insert(key, table.clone());
            }
        }
    }

    // Before the guard is dropped, because it writes outside this table: the
    // constraints pointing at a column this statement renamed follow it. After
    // the write-back, so the table's own self-referencing keys are updated
    // where they now live.
    let mut touched: Vec<(String, String)> = match renamed_columns.is_empty() {
        true => Vec::new(),
        false => rename_referenced_columns(&mut catalog, &db, &table_name, &renamed_columns),
    };

    // The table half of the same propagation, and #0205. After the write-back
    // too, and for the extra reason that a *self*-referencing key only points
    // at the new name once the table is living under it.
    //
    // The new name whole, dot and all, because that is the key the table now
    // lives under: `RENAME TO other.p3` puts it in this database under the
    // name `other.p3` rather than moving it, which is a gap of its own, and a
    // child naming anything else would point at nothing.
    if let Some(new_name) = &moved_to {
        touched.push((db.clone(), new_name.clone()));
        touched.extend(rename_referencing_tables(
            &mut catalog,
            &db,
            &table_name,
            new_name,
        ));
    }
    drop(catalog);
    for (database, name) in touched {
        engine.mark_modified_public(session, &database, &name);
    }

    Ok(Outcome::ok())
}

/// Every foreign key that points at `table_name`, with the table it is on.
///
/// This table's own keys included: a self-referencing key is on both sides at
/// once, and 8.3 answers a drop of the column it points *at* with the
/// referenced side's code, naming its own table. Those are read from `table` —
/// the copy an `ALTER TABLE` is building — so a key the statement dropped no
/// longer counts; every other table's from the catalog, since one statement
/// cannot have touched them.
fn keys_pointing_at<'a>(
    database: &'a Database,
    table_name: &str,
    table: &'a Table,
) -> Vec<(&'a str, &'a ForeignKey)> {
    database
        .tables
        .values()
        .flat_map(|other| {
            let keys = if other.name.eq_ignore_ascii_case(table_name) {
                &table.foreign_keys
            } else {
                &other.foreign_keys
            };
            // `other.name` and never the spelling the statement wrote: the
            // message reports the table as the catalog holds it, and for the
            // self case those two can differ in case.
            keys.iter()
                .filter(|fk| references_table(fk, &database.name, table_name))
                .map(move |fk| (other.name.as_str(), fk))
        })
        .collect()
}

/// Refuses a change that leaves some foreign key with no index behind it.
///
/// Both sides: a key needs an index over its own columns on the child, and one
/// over the columns it points at on the parent. Asked of the table as the
/// statement leaves it, which is what makes `DROP INDEX fkc, DROP FOREIGN KEY
/// fkc` and `DROP INDEX k2, ADD INDEX k3 (pid)` legal on 8.3 — a key that is
/// gone needs nothing, and a substitute added by the same statement counts.
///
/// The message names the index that was *dropped*, so the blame is looked up
/// rather than derived: the first of the indexes no longer there that would
/// have served. First by the table's own order and not the statement's, which
/// is what 8.3 reports — `DROP INDEX k1, DROP INDEX k2` and the same pair
/// reversed both name `k1`, `k1` being the one declared first.
///
/// Shared with the standalone `DROP INDEX ... ON t`, which reads the same rule
/// there: `DROP INDEX fk ON C` is 1553 on 8.3 exactly as the ALTER spelling is,
/// and that spelling walked away with it while this lived in `alter_table`.
pub(crate) fn refuse_unbacked_foreign_key(
    database: &Database,
    table_name: &str,
    table: &Table,
    original_indexes: &[Index],
    renames: &[(String, String)],
) -> EngineResult<()> {
    let referencing = keys_pointing_at(database, table_name, table);
    // The columns a key points at are read under the names this statement
    // leaves them, because the indexes over them already carry those names.
    // Asked under the old ones, `RENAME COLUMN u TO v` on a parent looked like
    // a drop of the index that backed the key and answered 1553, where 8.3
    // renames both sides and says nothing.
    let needed: Vec<Vec<String>> = table
        .foreign_keys
        .iter()
        .map(|fk| fk.columns.clone())
        .chain(
            referencing
                .iter()
                .map(|(_, fk)| after_renames(&fk.referenced_columns, renames)),
        )
        .collect();
    for columns in &needed {
        if served_by_some_index(table, columns) {
            continue;
        }
        // Gone means gone as it was: an index whose name the statement reused
        // for other columns took the old one's place, and the old one is what
        // the key has lost.
        let blamed = original_indexes
            .iter()
            .filter(|i| {
                !table.indexes.iter().any(|kept| {
                    kept.name.eq_ignore_ascii_case(&i.name) && kept.columns == i.columns
                })
            })
            .find(|i| index_serves(i, columns));
        // No dropped index to blame means the key lost its backing some other
        // way, and inventing an index name for that would be a worse answer
        // than the one this engine already gives.
        if let Some(index) = blamed {
            return Err(EngineError::drop_index_needed_by_fk(&index.name));
        }
    }
    Ok(())
}

/// Removes the named indexes, refusing a name the table has not got.
///
/// The standalone `DROP INDEX ... ON t` reaches this; the `ALTER TABLE`
/// spelling has its own arm. Through [`drop_indexes`] rather than a plain
/// `retain`, so `` DROP INDEX `PRIMARY` ON t `` clears the column flags with
/// the key instead of leaving a column reported `PRI` under no primary key.
pub(crate) fn drop_indexes_named(
    table: &mut Table,
    names: &[String],
    if_exists: bool,
) -> EngineResult<()> {
    for name in names {
        if !drop_indexes(table, |i| i.name.eq_ignore_ascii_case(name)) && !if_exists {
            // The same message the ALTER TABLE form gives, which is what 8.3
            // prints here too: `check that column/key exists`, not the `check
            // that it exists` this used to invent.
            return Err(EngineError::cant_drop(name));
        }
    }
    Ok(())
}

/// The plain column names a table constraint puts a key over.
///
/// A foreign key's referencing columns are in it and its *referenced* ones are
/// not: those name the parent's columns, which this table cannot be asked
/// about. A CHECK constrains no key at all.
///
/// Only the parts that are a bare column name, which is the difference between
/// this and [`index_column_names`]. That one renders anything else as text so
/// the index matches no column — a functional index over `(a + 1)`, and a
/// prefix index over `b(4)`, which is the shape Prisma generates for every
/// `TEXT` column it indexes. Asking `column_index` about the rendered form
/// would answer 1072 for both, where 8.3 accepts them.
fn key_columns_declared(constraint: &TableConstraint) -> Vec<String> {
    match constraint {
        TableConstraint::PrimaryKey(pk) => key_column_identifiers(&pk.columns),
        TableConstraint::Unique(unique) => key_column_identifiers(&unique.columns),
        TableConstraint::Index(index) => key_column_identifiers(&index.columns),
        TableConstraint::ForeignKey(fk) => fk.columns.iter().map(|c| c.value.clone()).collect(),
        _ => Vec::new(),
    }
}

/// The subset of an index's key parts that name a column and nothing else.
fn key_column_identifiers(columns: &[sqlparser::ast::IndexColumn]) -> Vec<String> {
    columns
        .iter()
        .filter_map(|c| match &c.column.expr {
            sqlparser::ast::Expr::Identifier(ident) => Some(ident.value.clone()),
            sqlparser::ast::Expr::CompoundIdentifier(parts) => {
                parts.last().map(|p| p.value.clone())
            }
            _ => None,
        })
        .collect()
}

/// The foreign keys an `ALTER TABLE` leaves the table holding.
///
/// The ones it drops are read off the statement rather than by applying it,
/// because the rule they feed is settled before the loop runs. Both spellings
/// of the drop count: `DROP FOREIGN KEY` and the `DROP CONSTRAINT` that
/// reaches the same key.
fn surviving_foreign_keys<'a>(
    table: &'a Table,
    operations: &[AlterTableOperation],
) -> Vec<&'a ForeignKey> {
    table
        .foreign_keys
        .iter()
        .filter(|fk| {
            !operations.iter().any(|operation| match operation {
                AlterTableOperation::DropForeignKey { name, .. }
                | AlterTableOperation::DropConstraint { name, .. } => {
                    fk.name.eq_ignore_ascii_case(&name.value)
                }
                _ => false,
            })
        })
        .collect()
}

/// What an `ALTER TABLE`'s column operations come to, walked in order.
struct ColumnWalk {
    /// The columns the statement takes away, under the names the table carried
    /// before it ran — which is how the indexes and the foreign keys spell
    /// them. A column renamed and *then* dropped is in here under its original
    /// name: `RENAME COLUMN pid TO qid, DROP COLUMN qid` takes `pid` away, and
    /// reading the drops against the original table alone missed that, so a
    /// foreign key over `pid` was left pointing at nothing.
    dropped: Vec<String>,
    /// Whether the operation loop refuses the statement first, for a reason
    /// 8.3 puts ahead of the foreign key rule hoisted above it.
    ///
    /// Two of them, and both were measured beating 1828 on 8.3:
    ///
    ///   * 1090, the last-column refusal. `DROP COLUMN pid, DROP COLUMN id,
    ///     DROP COLUMN x` on a three-column child is 1090 there and not 1828.
    ///   * 1054, a *reference* to a column that is not there, which on 8.3
    ///     outranks everything an `ALTER TABLE` could report — 1091, 1090,
    ///     1061 and 1072 were each measured losing to it, both ways round.
    ///
    /// Skipping the rule rather than reordering the loop is what keeps the
    /// rest of the statement's answers where they were: nothing here moves a
    /// refusal, it only declines to get in front of one.
    answered_first: bool,
}

/// Walks an `ALTER TABLE`'s column operations, in the statement's order.
///
/// Over the columns as the loop leaves them, and *that loop's* answers are
/// what this predicts rather than MySQL's. The two differ over a column the
/// same statement added or renamed — the loop resolves `ADD COLUMN z INT,
/// DROP COLUMN pid, MODIFY z INT` where 8.3 answers it 1054, and drops `qid`
/// in the rename above where 8.3 answers 1091, each a divergence of its own —
/// and asking the original table here would have waved the foreign key rule
/// off those statements in favour of a refusal the loop then does not raise,
/// leaving the key's column dropped. A walk that predicts the loop cannot do
/// that: it declines only where a refusal really is coming.
fn walk_columns(table: &Table, operations: &[AlterTableOperation]) -> ColumnWalk {
    // Each live column as it is spelled now, beside the name the statement
    // found it under — `None` for one the statement itself added, which no key
    // that existed before it can name.
    let mut live: Vec<(&str, Option<&str>)> = table
        .columns
        .iter()
        .map(|c| (c.name.as_str(), Some(c.name.as_str())))
        .collect();
    let mut dropped: Vec<String> = Vec::new();
    let position = |live: &[(&str, Option<&str>)], name: &str| {
        live.iter().position(|(c, _)| c.eq_ignore_ascii_case(name))
    };
    for operation in operations {
        match operation {
            AlterTableOperation::AddColumn { column_def, .. } => {
                // A name already there is 1060, which does *not* precede the
                // rule — so the column list is left as it is and the loop
                // answers whichever of the two comes first.
                if position(&live, &column_def.name.value).is_none() {
                    live.push((&column_def.name.value, None));
                }
            }
            AlterTableOperation::DropColumn { column_names, .. } => {
                for name in column_names {
                    // A name the table has not got is 1091, which loses to the
                    // rule, so it is passed over rather than answered here.
                    let Some(at) = position(&live, &name.value) else {
                        continue;
                    };
                    if live.len() == 1 {
                        return ColumnWalk {
                            dropped,
                            answered_first: true,
                        };
                    }
                    if let (_, Some(original)) = live.remove(at) {
                        if !dropped.iter().any(|d| d.eq_ignore_ascii_case(original)) {
                            dropped.push(original.to_owned());
                        }
                    }
                }
            }
            AlterTableOperation::RenameColumn {
                old_column_name,
                new_column_name,
            } => match position(&live, &old_column_name.value) {
                Some(at) => live[at].0 = &new_column_name.value,
                None => {
                    return ColumnWalk {
                        dropped,
                        answered_first: true,
                    }
                }
            },
            AlterTableOperation::ChangeColumn {
                old_name, new_name, ..
            } => match position(&live, &old_name.value) {
                Some(at) => live[at].0 = &new_name.value,
                None => {
                    return ColumnWalk {
                        dropped,
                        answered_first: true,
                    }
                }
            },
            AlterTableOperation::ModifyColumn { col_name, .. }
                if position(&live, &col_name.value).is_none() =>
            {
                return ColumnWalk {
                    dropped,
                    answered_first: true,
                }
            }
            _ => {}
        }
    }
    ColumnWalk {
        dropped,
        answered_first: false,
    }
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

    let columns = index_column_names(&create.columns);
    let index = Index {
        name,
        columns,
        unique: create.unique,
        primary: false,
        implicit_for_foreign_key: false,
    };

    // Which provisional indexes this one would take over, worked out and not
    // applied: this path checks the name itself before reaching `add_index`,
    // for the ordering reason below, and a name held by an index about to be
    // taken over is not taken. Nothing is removed here, because the checks
    // that follow can still refuse and this path holds the live catalog rather
    // than a clone. #0255.
    let superseded = provisional_indexes_superseded_by(table, &index);

    // After the takeover is worked out, not before it. A name held only by an
    // index about to be taken over is not a reason to report success and
    // create nothing: measured, doing that told the client the index existed
    // and then let a later `CREATE INDEX` over the same column delete the one
    // it had been shown.
    if create.if_not_exists && index_name_taken_apart_from(table, &index.name, &superseded) {
        return Ok(Outcome::ok());
    }

    // Before the data is walked, which is the order 8.3 answers in: a
    // `CREATE UNIQUE INDEX` under a name the table already carries, over a
    // column that already holds duplicates, is 1061 there and not the 1062 the
    // rows would raise. A duplicate name also beats a column that does not
    // exist at all, so nothing about what the index covers can be the first
    // answer.
    if index_name_taken_apart_from(table, &index.name, &superseded) {
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
