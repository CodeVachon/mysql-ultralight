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
/// The same tables with their columns and no rows, for describing a statement.
///
/// Built by asking for the real thing against an empty catalog rather than
/// keeping a second list of column names. Two lists would drift, and a
/// describe that declares a column the query does not return is the exact
/// failure this is meant to prevent.
pub fn shape(session: &Session, name: &str) -> EngineResult<Table> {
    let mut shape = table(&Catalog::new(), session, name)?;
    shape.rows.clear();
    Ok(shape)
}

pub fn table(catalog: &Catalog, session: &Session, name: &str) -> EngineResult<Table> {
    let lower = name.to_ascii_lowercase();
    let mut table = built(catalog, session, &lower)?;
    collate(&mut table, session);
    Ok(table)
}

/// The table before its text is collated. See [`collate`].
fn built(catalog: &Catalog, session: &Session, lower: &str) -> EngineResult<Table> {
    Ok(match lower {
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
        "views" => views(catalog),
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

/// One `information_schema` table, with each column declared the way MySQL
/// declares it where [`COLUMN_SPEC`] records that.
fn build(name: &str, headers: &[&str], rows: Vec<Vec<Value>>) -> Table {
    let mut table = Table::new(name);
    table.columns = headers
        .iter()
        .enumerate()
        .map(|(i, header)| {
            // Declared as MySQL declares it, where that has been measured.
            // These columns are read by tools, not people: Prisma's schema
            // describer asks `information_schema.statistics` for
            // `seq_in_index` and unwraps it as an integer, so handing back the
            // string "1" crashes `prisma db pull` and `prisma migrate dev`.
            let (ty, not_null) = match specified(name, header) {
                Some(found) => found,
                // Inference for anything not measured: a column whose values
                // are all integers is an integer. It is what every column used
                // to get, and it makes a column's type depend on the rows a
                // particular query happened to select.
                None => {
                    let numeric = rows
                        .iter()
                        .filter_map(|row| row.get(i))
                        .filter(|value| !value.is_null())
                        .all(|value| matches!(value, Value::Int(_)));
                    let has_values = rows
                        .iter()
                        .filter_map(|row| row.get(i))
                        .any(|value| !value.is_null());
                    let inferred = if numeric && has_values {
                        integer_type()
                    } else {
                        text_type()
                    };
                    (inferred, false)
                }
            };

            let mut column = Column::new(*header, ty);
            column.nullable = !not_null;
            column.position = i + 1;
            column
        })
        .collect();
    table.rows = rows;
    table
}

/// Puts a byte comparison on the values of the columns MySQL declares
/// `utf8mb3_bin`.
///
/// Names in `information_schema` are case-*sensitive* on 8.3, and this engine
/// compared them the way it compares everything else. Two wrong answers,
/// reachable by any client that introspects the catalog before it emits DDL —
/// which is what Prisma does. Measured on 8.3 against tables `apple`, `Bee`
/// and `cherry`, with the connection collated `utf8mb4_0900_ai_ci`:
///
/// ```text
///     ORDER BY TABLE_NAME            mysql Bee,apple,cherry   engine apple,Bee,cherry
///     WHERE TABLE_NAME = 'bee'       mysql no rows            engine Bee
///     WHERE TABLE_NAME LIKE 'b%'     mysql no rows            engine Bee
///     WHERE TABLE_SCHEMA = 'PROBE'   mysql 0                  engine 3
///     WHERE SCHEMA_NAME = 'PROBE'    mysql no rows            engine probe
///     WHERE DATA_TYPE = 'INT'        mysql 0                  engine 7
///     WHERE TABLE_TYPE = 'base table' mysql 0                 engine 3
///     WHERE TABLE_NAME = 'Bee '      mysql Bee                engine no rows
/// ```
///
/// The last is the other half of the same fact and the reason this is a
/// collation rather than a `case_sensitive` flag: `utf8mb3_bin` is PAD SPACE,
/// so a trailing space is ignored where the connection's NO PAD
/// `utf8mb4_0900_ai_ci` counts it. [`crate::collation::Collation::Bin`] pads,
/// so both halves come from the one change.
///
/// **Not every text column, and the exceptions are measured, not guessed.**
/// Reading `information_schema.COLUMNS` on a stock 8.3 for the twelve tables
/// this engine serves gives three collations: `utf8mb3_bin` for the names of
/// schemas, tables and types; `utf8mb3_tolower_ci` for `COLUMN_NAME`,
/// `INDEX_NAME`, `CONSTRAINT_NAME`, `UNIQUE_CONSTRAINT_NAME` and
/// `REFERENCED_COLUMN_NAME`; and `utf8mb3_general_ci` for the rest —
/// `IS_NULLABLE`, `EXTRA`, `COLLATION_NAME`, `ENGINE`, `TABLE_COMMENT`. Both
/// of the latter two are case-*insensitive*, which is what this engine already
/// does, so only the first set changes. `COLUMN_SPEC`'s trailing ` binary`
/// marks exactly that set and no other, which was checked column by column
/// against the measurement.
///
/// The values and not the declaration, deliberately. `SqlType::collation` is
/// what `info_schema::collation_name` reads to tell `information_schema`'s own
/// `utf8mb3_bin` columns from a user column resolved to `utf8mb4_bin`, and
/// 8.3 reports those two names differently — `CREATE TABLE c AS SELECT
/// COLUMN_TYPE FROM information_schema.COLUMNS` gives a `utf8mb3_bin` column
/// there. Moving the declaration would have made the copy claim `utf8mb4_bin`,
/// trading this wrong answer for that one.
///
/// Every other text column keeps the coercibility it had and gains the
/// *connection's* collation, which is where it was already being compared. Not
/// a change of answer: it is what a literal now carries too, so the two go on
/// resolving to one collation instead of to none. Without it a
/// `utf8mb4_general_ci` connection asking `WHERE ENGINE = 'InnoDB'` met a
/// literal collated `utf8mb4_general_ci` beside a row value still carrying the
/// server default, and this engine raised 1267 for a pair 8.3 compares
/// happily.
///
/// COERCIBLE rather than IMPLICIT, and that is measured rather than tidy. 8.3
/// declares these columns `utf8mb3_general_ci` and `utf8mb3_tolower_ci`, but
/// `information_schema` there is a set of *views*, and a column that is an
/// expression in one behaves as an item and not as a column. Both appear:
/// with the connection collated `utf8mb4_0900_ai_ci`, `WHERE ENGINE =
/// 'InnoDB  '` matches on 8.3 — the trailing spaces pad, so the column's own
/// PAD SPACE collation decided it — while `WHERE IS_NULLABLE = 'NO '` matches
/// nothing, which is the connection's NO PAD deciding instead. Two columns of
/// the same declared collation in the same table, so which of them is an
/// expression is a table of MySQL's view definitions rather than a rule this
/// engine could apply. Leaving them all COERCIBLE keeps the second answer
/// right and the first wrong, which is where they already were.
fn collate(table: &mut Table, session: &Session) {
    let connection = crate::collation::Collated::coercible(session.collation_connection());
    let binary = crate::collation::Collated::implicit(crate::collation::Collation::Bin);
    for (i, column) in table.columns.iter().enumerate() {
        let want = if column.ty.binary_collation {
            binary
        } else {
            connection
        };
        for row in &mut table.rows {
            if let Some(Value::Text(_, collated)) = row.get_mut(i) {
                *collated = want;
            }
        }
    }
}

/// How MySQL describes each `information_schema` column on the wire.
///
type ColumnSpec = (&'static str, &'static str, bool);

/// One `information_schema` table and the columns this engine serves from it.
type TableSpec = (&'static str, &'static [ColumnSpec]);

/// Taken from what MySQL 8.3 declares its own columns as, rather than inferred
/// from the rows — which is what this used to do. Inference is wrong twice
/// over: it calls a column an integer because the rows a particular query
/// happened to select are integers, so the type moves with the data, and it can
/// only ever produce the two types it knows. `NUMERIC_PRECISION` is a
/// `bigint unsigned` where every integer here was a signed one, and
/// `COLUMN_TYPE` is text that MySQL sends as a blob.
///
/// MySQL does not answer this consistently itself, so exact agreement is not
/// available: reading `information_schema` narrows a column to the smallest
/// type its rows need, and materialising the result — which `ORDER BY
/// ORDINAL_POSITION` forces, and which every introspection query does — falls
/// back to the declared one. `NUMERIC_PRECISION` is a `LONG` under a plain
/// scan and a `LONGLONG` under that `ORDER BY`. The declared type is what a
/// describing tool sees, so it is what is recorded here; the two text columns
/// that narrow even when materialised are spelled as they arrive.
///
/// The spelling carries the sign, as a declared type does everywhere else, and
/// a trailing ` binary` carries the collation — MySQL declares most of these
/// columns `utf8mb3_bin`, which is not part of `COLUMN_TYPE` but does put the
/// BINARY flag on the wire, and a client that introspects a schema reads every
/// one of them. The flag after the spelling is NOT NULL.
///
/// Only the columns this engine actually serves are here; a name absent from
/// the table falls back to inference rather than being invented.
/// One column: its name, the type MySQL declares it with, and whether MySQL
/// marks it NOT NULL.
const COLUMN_SPEC: &[TableSpec] = &[
    (
        "CHARACTER_SETS",
        &[
            ("CHARACTER_SET_NAME", "varchar(64)", true),
            ("DEFAULT_COLLATE_NAME", "varchar(64)", true),
            ("DESCRIPTION", "varchar(2048)", true),
            ("MAXLEN", "int unsigned", true),
        ],
    ),
    (
        "COLLATIONS",
        &[
            ("COLLATION_NAME", "varchar(64)", true),
            ("CHARACTER_SET_NAME", "varchar(64)", true),
            ("ID", "bigint unsigned", true),
            ("IS_DEFAULT", "varchar(3)", true),
            ("IS_COMPILED", "varchar(3)", true),
            ("SORTLEN", "int unsigned", true),
        ],
    ),
    (
        "COLUMNS",
        &[
            ("TABLE_CATALOG", "varchar(64) binary", true),
            ("TABLE_SCHEMA", "varchar(64) binary", true),
            ("TABLE_NAME", "varchar(64) binary", true),
            ("COLUMN_NAME", "varchar(64)", false),
            ("ORDINAL_POSITION", "int unsigned", true),
            ("COLUMN_DEFAULT", "text binary", false),
            ("IS_NULLABLE", "varchar(3)", true),
            ("DATA_TYPE", "text binary", false),
            ("CHARACTER_MAXIMUM_LENGTH", "bigint", false),
            ("NUMERIC_PRECISION", "bigint unsigned", false),
            ("NUMERIC_SCALE", "bigint unsigned", false),
            ("DATETIME_PRECISION", "int unsigned", false),
            ("COLUMN_TYPE", "mediumtext binary", true),
            // An ENUM rather than a string, and the only one in this table:
            // MySQL declares it `enum('','PRI','UNI','MUL')` and sends it with
            // ENUM_FLAG, so a client introspecting a schema through this
            // column reads it as a member list. The width is the same either
            // way — the widest member is three characters, which is what
            // `char(3)` said.
            ("COLUMN_KEY", "enum('','PRI','UNI','MUL') binary", true),
            ("EXTRA", "varchar(256)", false),
            ("COLLATION_NAME", "varchar(64)", false),
            ("COLUMN_COMMENT", "text binary", true),
        ],
    ),
    (
        "ENGINES",
        &[
            ("ENGINE", "varchar(64)", true),
            ("SUPPORT", "varchar(8)", true),
            ("COMMENT", "varchar(80)", true),
            ("TRANSACTIONS", "varchar(3)", false),
            ("XA", "varchar(3)", false),
            ("SAVEPOINTS", "varchar(3)", false),
        ],
    ),
    (
        "KEY_COLUMN_USAGE",
        &[
            ("CONSTRAINT_CATALOG", "varchar(64) binary", true),
            ("CONSTRAINT_SCHEMA", "varchar(64) binary", true),
            ("CONSTRAINT_NAME", "varchar(64)", false),
            ("TABLE_SCHEMA", "varchar(64) binary", true),
            ("TABLE_NAME", "varchar(64) binary", true),
            ("COLUMN_NAME", "varchar(64)", false),
            ("ORDINAL_POSITION", "int unsigned", true),
            ("POSITION_IN_UNIQUE_CONSTRAINT", "int unsigned", false),
            ("REFERENCED_TABLE_SCHEMA", "varchar(64) binary", false),
            ("REFERENCED_TABLE_NAME", "varchar(64) binary", false),
            ("REFERENCED_COLUMN_NAME", "varchar(64)", false),
        ],
    ),
    (
        "PROCESSLIST",
        &[
            ("ID", "bigint unsigned", true),
            ("USER", "varchar(32)", true),
            ("HOST", "varchar(261)", true),
            ("DB", "varchar(64)", false),
            ("COMMAND", "varchar(16)", true),
            ("TIME", "int", true),
            ("STATE", "varchar(64)", false),
            ("INFO", "varchar(65535)", false),
        ],
    ),
    (
        "REFERENTIAL_CONSTRAINTS",
        &[
            ("CONSTRAINT_CATALOG", "varchar(64) binary", true),
            ("CONSTRAINT_SCHEMA", "varchar(64) binary", true),
            ("CONSTRAINT_NAME", "varchar(64)", false),
            ("UNIQUE_CONSTRAINT_CATALOG", "varchar(64) binary", true),
            ("UNIQUE_CONSTRAINT_SCHEMA", "varchar(64) binary", true),
            ("UNIQUE_CONSTRAINT_NAME", "varchar(64)", false),
            ("MATCH_OPTION", "char(7) binary", true),
            ("UPDATE_RULE", "char(11) binary", true),
            ("DELETE_RULE", "char(11) binary", true),
            ("TABLE_NAME", "varchar(64) binary", true),
            ("REFERENCED_TABLE_NAME", "varchar(64) binary", true),
        ],
    ),
    (
        "SCHEMATA",
        &[
            ("CATALOG_NAME", "varchar(64) binary", true),
            ("SCHEMA_NAME", "varchar(64) binary", true),
            ("DEFAULT_CHARACTER_SET_NAME", "varchar(64)", true),
            ("DEFAULT_COLLATION_NAME", "varchar(64)", true),
            ("SQL_PATH", "binary(0)", false),
        ],
    ),
    (
        "STATISTICS",
        &[
            ("TABLE_CATALOG", "varchar(64) binary", true),
            ("TABLE_SCHEMA", "varchar(64) binary", true),
            ("TABLE_NAME", "varchar(64) binary", true),
            ("NON_UNIQUE", "int", true),
            ("INDEX_SCHEMA", "varchar(64) binary", true),
            ("INDEX_NAME", "varchar(64)", false),
            ("SEQ_IN_INDEX", "int unsigned", true),
            ("COLUMN_NAME", "varchar(64)", false),
            ("COLLATION", "varchar(1)", false),
            ("CARDINALITY", "bigint", false),
            ("SUB_PART", "bigint", false),
            ("INDEX_TYPE", "varchar(11) binary", true),
        ],
    ),
    (
        "TABLES",
        &[
            ("TABLE_CATALOG", "varchar(64) binary", true),
            ("TABLE_SCHEMA", "varchar(64) binary", true),
            ("TABLE_NAME", "varchar(64) binary", true),
            ("TABLE_TYPE", "char(11) binary", true),
            ("ENGINE", "varchar(64)", false),
            ("VERSION", "int", false),
            ("ROW_FORMAT", "char(10) binary", false),
            ("TABLE_ROWS", "bigint unsigned", false),
            ("TABLE_COLLATION", "varchar(64)", false),
            ("TABLE_COMMENT", "text", false),
            ("CREATE_OPTIONS", "varchar(256)", false),
        ],
    ),
    (
        "TABLE_CONSTRAINTS",
        &[
            ("CONSTRAINT_CATALOG", "varchar(64) binary", true),
            ("CONSTRAINT_SCHEMA", "varchar(64) binary", true),
            ("CONSTRAINT_NAME", "varchar(64)", false),
            ("TABLE_SCHEMA", "varchar(64) binary", true),
            ("TABLE_NAME", "varchar(64) binary", true),
            ("CONSTRAINT_TYPE", "varchar(11) binary", true),
            ("ENFORCED", "varchar(3) binary", true),
        ],
    ),
    (
        "VIEWS",
        &[
            ("TABLE_CATALOG", "varchar(64) binary", true),
            ("TABLE_SCHEMA", "varchar(64) binary", true),
            ("TABLE_NAME", "varchar(64) binary", true),
            ("VIEW_DEFINITION", "longtext binary", false),
            ("CHECK_OPTION", "char(8) binary", false),
            ("IS_UPDATABLE", "char(3) binary", false),
            ("DEFINER", "varchar(288) binary", false),
            ("SECURITY_TYPE", "varchar(7) binary", false),
            ("CHARACTER_SET_CLIENT", "varchar(64)", true),
            ("COLLATION_CONNECTION", "varchar(64)", true),
        ],
    ),
];

/// The declared type and nullability MySQL gives one `information_schema`
/// column, if this is one it describes.
fn specified(table: &str, column: &str) -> Option<(SqlType, bool)> {
    let columns = COLUMN_SPEC
        .iter()
        .find(|(name, _)| *name == table)
        .map(|(_, columns)| *columns)?;
    let (_, spelling, not_null) = columns.iter().find(|(name, ..)| *name == column)?;
    Some((spec_type(spelling)?, *not_null))
}

/// `COLLATION_NAME`, which only a column that holds *characters* has.
///
/// NULL otherwise, and that is not a detail: MySQL reports NULL for an INT, a
/// DATE, a DECIMAL, a JSON — and for a BLOB, a BINARY and a VARBINARY, which
/// hold bytes and carry no character set at all. Measured on 8.3 across all
/// twelve; only `char`/`varchar`/`text`, `enum` and `set` report one. This
/// reported the database's collation for every column of every type, so an
/// `INT` claimed `utf8mb4_0900_ai_ci`.
///
/// The binary collation comes first, because it is the one collation this
/// engine actually *models*: `SqlType::binary_collation` puts the BINARY flag
/// on the wire definition, and reporting the database's collation beside it
/// made one column describe itself two ways. The reachable case is
/// `CREATE TABLE c AS SELECT COLUMN_TYPE FROM information_schema.COLUMNS`,
/// whose copy 8.3 declares `utf8mb3_bin` — and `tests/image.rs` already exists
/// to protect that flag on exactly this column.
///
/// Then the three declarations, narrowest first, which is 8.3's own order: a
/// column's own `COLLATE` wins over its table's, and the table's over the
/// database's. All three measured — a table-level clause shows up on every
/// character column of the table, and a database-level one on every character
/// column of every table that declares nothing.
fn collation_name(
    ty: &SqlType,
    declared: Option<&str>,
    table: Option<&str>,
    database: &str,
) -> Value {
    match ty.kind {
        TypeKind::Text | TypeKind::Enum | TypeKind::Set => {
            // `binary_collation` is the *wire* flag, and two different things
            // set it: `information_schema` declares its own text columns
            // `utf8mb3_bin`, which this engine reports and cannot compare in,
            // and a resolved `utf8mb4_bin` puts the same flag on a user
            // column. Only the first is this name — 8.3 reports
            // `utf8mb4_bin` for the second — so the flag alone cannot decide
            // it and the resolved collation is what separates them. Asked of
            // the resolution rather than of the clause, because the clause can
            // be on the *table*: `CREATE TABLE t (a VARCHAR(10))
            // COLLATE=utf8mb4_bin` reported `utf8mb3_bin` while comparing `a`
            // as bytes, which is one column describing itself two ways.
            if ty.binary_collation && ty.collation != crate::collation::Collation::Bin {
                return text(crate::collation::BINARY);
            }
            // The same reader `SqlType::collation` resolved the *comparison*
            // through, so what a column reports and what it compares as
            // cannot drift apart.
            text(crate::collation::inherited(declared, table, database))
        }
        _ => Value::Null,
    }
}

/// A declared type from the spelling the spec writes it as.
///
/// The same spellings a `CREATE TABLE` would use, so `SqlType` reports them the
/// way it reports any other column — including `int unsigned`, which is the
/// form `COLUMN_TYPE` itself uses, and `text binary`, which is how MySQL's own
/// DDL asks for the binary collation of a character set. The collation is not
/// part of `COLUMN_TYPE`, so it is stripped back off the spelling: a client
/// asking what these columns are declared as reads `mediumtext`.
fn spec_type(spelling: &str) -> Option<SqlType> {
    let (spelling, binary_collation) = match spelling.strip_suffix(" binary") {
        Some(base) => (base, true),
        None => (spelling, false),
    };
    let (base, unsigned) = match spelling.strip_suffix(" unsigned") {
        Some(base) => (base, true),
        None => (spelling, false),
    };
    let (name, argument) = match base.split_once('(') {
        Some((name, rest)) => (name, Some(rest.trim_end_matches(')'))),
        None => (base, None),
    };
    // An ENUM's parenthesis holds its members and every other type's holds a
    // number, so the two are read apart rather than both through `parse`:
    // `enum('','PRI')` would otherwise be a type with no length and no
    // members, which is an ENUM whose widest member is nothing — and the width
    // is what `column_length` is computed from.
    let (length, variants) = match name {
        "enum" => (
            None,
            argument
                .unwrap_or_default()
                .split(',')
                .map(|member| member.trim().trim_matches('\'').to_owned())
                .collect(),
        ),
        _ => (argument.and_then(|a| a.parse().ok()), Vec::new()),
    };
    let kind = match name {
        "int" | "bigint" => TypeKind::Int,
        "varchar" | "char" | "text" | "longtext" | "mediumtext" => TypeKind::Text,
        // `SCHEMATA.SQL_PATH`, which MySQL declares `binary(0)` and always
        // answers NULL for.
        "binary" => TypeKind::Blob,
        "timestamp" | "datetime" => TypeKind::DateTime,
        // `COLUMNS.COLUMN_KEY`, which is a real ENUM on 8.3 and reaches the
        // wire with ENUM_FLAG.
        "enum" => TypeKind::Enum,
        _ => return None,
    };
    Some(SqlType {
        kind,
        declared: spelling.to_owned(),
        length,
        scale: None,
        unsigned,
        variants,
        binary_collation,
        collation: crate::collation::Collation::Ai0900Ci,
    })
}

fn integer_type() -> SqlType {
    SqlType {
        kind: TypeKind::Int,
        declared: "bigint".to_owned(),
        length: None,
        scale: None,
        unsigned: false,
        variants: Vec::new(),
        binary_collation: false,
        collation: crate::collation::Collation::Ai0900Ci,
    }
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
        binary_collation: false,
        collation: crate::collation::Collation::Ai0900Ci,
    }
}

fn text(s: impl Into<String>) -> Value {
    Value::text(s.into())
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
    // A view appears here too, with TABLE_TYPE = VIEW.
    for name in catalog.database_names() {
        let Some(db) = catalog.database(&name) else {
            continue;
        };
        for view in db.views.values() {
            rows.push(vec![
                text("def"),
                text(&name),
                text(&view.name),
                text("VIEW"),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                text("VIEW"),
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
                rows.push(vec![
                    text("def"),
                    text(&name),
                    text(&table.name),
                    text(&column.name),
                    Value::Int(column.position as i64),
                    // COLUMN_DEFAULT distinguishes "no default" (NULL) from a
                    // default of the string "NULL".
                    match (&column.default, &column.default_expr) {
                        (Some(v), _) => Value::text(v.as_text()),
                        (None, Some(e)) => Value::text(e.clone()),
                        (None, None) => Value::Null,
                    },
                    text(if column.nullable { "YES" } else { "NO" }),
                    text(column.ty.base_name()),
                    character_maximum_length(&column.ty),
                    numeric_precision(&column.ty),
                    numeric_scale(&column.ty),
                    datetime_precision(&column.ty),
                    text(&column.ty.declared),
                    // MySQL also reports MUL for the first column of any other
                    // index, which is how a describing tool learns that a
                    // compound unique or a secondary index exists at all.
                    text(if column.primary_key {
                        "PRI"
                    } else if column.unique {
                        "UNI"
                    } else if table.indexes.iter().any(|index| {
                        index
                            .columns
                            .first()
                            .is_some_and(|first| first.eq_ignore_ascii_case(&column.name))
                    }) {
                        "MUL"
                    } else {
                        ""
                    }),
                    text(if column.auto_increment {
                        "auto_increment"
                    } else {
                        ""
                    }),
                    collation_name(
                        &column.ty,
                        column.collation.as_deref(),
                        table.collation.as_deref(),
                        &db.collation,
                    ),
                    text(column.comment.clone().unwrap_or_default()),
                ]);
            }
        }
    }
    // A view's columns come from the shape resolved when it was created.
    // Prisma finds a view in TABLES and then reads COLUMNS for it, so omitting
    // these presents a zero-column object.
    for name in catalog.database_names() {
        let Some(db) = catalog.database(&name) else {
            continue;
        };
        for view in db.views.values() {
            for column in &view.shape {
                rows.push(vec![
                    text("def"),
                    text(&name),
                    text(&view.name),
                    text(&column.name),
                    Value::Int(column.position as i64),
                    Value::Null,
                    text(if column.nullable { "YES" } else { "NO" }),
                    text(column.ty.base_name()),
                    // Described exactly as a table's column is: a view's
                    // columns carry real types, and reporting nothing for them
                    // makes a describing tool see a type-less column.
                    character_maximum_length(&column.ty),
                    numeric_precision(&column.ty),
                    numeric_scale(&column.ty),
                    datetime_precision(&column.ty),
                    text(&column.ty.declared),
                    text(""),
                    text(""),
                    collation_name(&column.ty, column.collation.as_deref(), None, &db.collation),
                    text(""),
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

/// `CHARACTER_MAXIMUM_LENGTH`, which MySQL reports for every string type.
///
/// A `TEXT` column has a maximum whether or not the DDL wrote one, and an ENUM's
/// is the length of its longest member. Reporting NULL instead is not merely
/// incomplete: Prisma's schema describer unwraps this for a text column, so a
/// missing value crashes `prisma migrate dev` and `prisma db pull` — which is
/// how this was found.
fn character_maximum_length(ty: &crate::types::SqlType) -> Value {
    let bytes = match ty.kind {
        TypeKind::Text => match ty.length {
            Some(length) => length,
            // The width implied by the spelling, as MySQL stores it.
            None => match ty.base_name().as_str() {
                "tinytext" => 255,
                "mediumtext" => 16_777_215,
                "longtext" => 4_294_967_295,
                _ => 65_535,
            },
        },
        TypeKind::Blob => match ty.length {
            // `BINARY(4)` and `VARBINARY(9)` are blobs here, and their length
            // is the declared one rather than the width of the spelling.
            Some(length) => length,
            None => match ty.base_name().as_str() {
                "tinyblob" => 255,
                "mediumblob" => 16_777_215,
                "longblob" => 4_294_967_295,
                _ => 65_535,
            },
        },
        // The longest value it can hold, which for a SET is every member plus
        // the commas between them.
        TypeKind::Enum => ty
            .variants
            .iter()
            .map(|v| v.chars().count())
            .max()
            .unwrap_or(0) as u64,
        TypeKind::Set => {
            let members: usize = ty.variants.iter().map(|v| v.chars().count()).sum();
            (members + ty.variants.len().saturating_sub(1)) as u64
        }
        TypeKind::Json => return Value::Null,
        _ => return Value::Null,
    };
    Value::Int(bytes as i64)
}

/// `DATETIME_PRECISION`, the fractional-second digits a temporal type carries.
///
/// A `DATE` has none, and MySQL reports NULL rather than 0 — it carries no
/// fractional seconds at all, where a `TIMESTAMP` carries zero of them. This
/// said 0 for both, on the strength of a comment written from memory; the
/// differential harness asked MySQL.
fn datetime_precision(ty: &crate::types::SqlType) -> Value {
    match ty.kind {
        TypeKind::DateTime | TypeKind::Time => Value::Int(ty.length.unwrap_or(0) as i64),
        _ => Value::Null,
    }
}

/// `NUMERIC_SCALE`, which MySQL reports for every exact numeric type.
///
/// One function rather than the rule written out at each caller: a table's
/// columns and a view's are described by separate loops, and when each carried
/// its own copy they drifted — a `BOOLEAN` reported scale 0 through a view and
/// nothing through the table it came from.
///
/// A `FLOAT` or `DOUBLE` is the exception: it has a scale only when the DDL
/// wrote one, because an approximate type without `(M,D)` has no fixed number
/// of decimal places to report. MySQL answers NULL there.
fn numeric_scale(ty: &crate::types::SqlType) -> Value {
    match ty.kind {
        TypeKind::Int | TypeKind::Decimal | TypeKind::Bool => {
            Value::Int(ty.scale.unwrap_or(0) as i64)
        }
        TypeKind::Float => match ty.scale {
            Some(scale) => Value::Int(scale as i64),
            None => Value::Null,
        },
        _ => Value::Null,
    }
}

/// `NUMERIC_PRECISION`, which MySQL reports for every numeric type.
///
/// An `INT` has a precision of 10 whether or not the DDL wrote a display width,
/// and describing tools read it as a given.
fn numeric_precision(ty: &crate::types::SqlType) -> Value {
    let precision = match ty.kind {
        // A DECIMAL carries its own, and defaults to MySQL's 10 without one.
        TypeKind::Decimal => ty.length.unwrap_or(10),
        // A `FLOAT(7,3)` reports the 7 it was declared with; only a bare
        // `FLOAT` or `DOUBLE` falls back to the width of the storage.
        TypeKind::Float => match ty.length {
            Some(precision) => precision,
            None if ty.base_name() == "float" => 12,
            None => 22,
        },
        TypeKind::Int => match ty.base_name().as_str() {
            "tinyint" => 3,
            "smallint" => 5,
            "mediumint" => 7,
            "bigint" => {
                if ty.unsigned {
                    20
                } else {
                    19
                }
            }
            _ => 10,
        },
        // BOOLEAN is TINYINT(1) underneath.
        TypeKind::Bool => 3,
        _ => return Value::Null,
    };
    Value::Int(precision as i64)
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

fn views(catalog: &Catalog) -> Table {
    let mut rows = Vec::new();
    for name in catalog.database_names() {
        let Some(db) = catalog.database(&name) else {
            continue;
        };
        for view in db.views.values() {
            rows.push(vec![
                text("def"),
                text(&name),
                text(&view.name),
                // The definition is what the view's query renders to, which
                // for a compound `EXTRACT` is the call `crate::rewrite` turned
                // it into. Undone here rather than in the catalog: the stored
                // text is re-parsed to refresh the view and to read the state
                // image back, and only this row is read by a client.
                text(crate::rewrite::extract_as_written(&view.definition)),
                text("NONE"),
                // Nothing here is updatable: a view is materialised on read.
                text("NO"),
                text("root@localhost"),
                text("DEFINER"),
                text("utf8mb4"),
                text("utf8mb4_0900_ai_ci"),
            ]);
        }
    }
    build(
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

/// `information_schema.COLLATIONS`, built from the list a `COLLATE` clause is
/// checked against.
///
/// Built from it rather than written out beside it: this table saying a
/// collation exists and the clause refusing it — or the reverse — is a server
/// contradicting itself about its own capabilities, and the two were separate
/// lists until `collation::KNOWN` became the single one.
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
        crate::collation::KNOWN
            .iter()
            .map(|c| {
                vec![
                    text(c.name),
                    text(c.charset),
                    Value::Int(c.id),
                    text(if c.is_default { "Yes" } else { "" }),
                    // Every collation this engine has is built in; there is no
                    // dynamic loading for the column to distinguish.
                    text("Yes"),
                    Value::Int(c.sortlen),
                ]
            })
            .collect(),
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
                .map(Value::text)
                .unwrap_or(Value::Null),
            text("Query"),
            Value::Int(0),
            text("executing"),
            Value::Null,
        ]],
    )
}

#[cfg(test)]
mod spec_tests {
    use super::{spec_type, COLUMN_SPEC};

    /// Every spelling in the table has to parse.
    ///
    /// `specified` falls back to inference when one does not, which is silent:
    /// a typo would put that column back on the type-from-the-rows guessing
    /// this table exists to replace, and nothing would report it.
    #[test]
    fn every_spelling_is_understood() {
        for (table, columns) in COLUMN_SPEC {
            for (column, spelling, _) in *columns {
                assert!(
                    spec_type(spelling).is_some(),
                    "{table}.{column} is spelled `{spelling}`, which spec_type does not read"
                );
            }
        }
    }
}
