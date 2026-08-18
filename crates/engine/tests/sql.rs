//! End-to-end SQL tests against the engine, with no socket in the way.

use mysql_ultralight_engine::{Engine, Outcome, ServerConfig, Session, Value};

/// A fresh engine with one database selected.
fn engine() -> (std::sync::Arc<Engine>, Session) {
    let engine = Engine::new(ServerConfig::default());
    engine.bootstrap_database("testdb").unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    (engine, session)
}

/// Runs a statement, panicking with the SQL on failure so a broken test names
/// what broke.
fn run(engine: &Engine, session: &mut Session, sql: &str) -> Outcome {
    engine
        .execute(session, sql, &[])
        .unwrap_or_else(|e| panic!("{sql}\n  -> {e}"))
}

/// Runs a query and returns its rows as text, for compact assertions.
fn rows(engine: &Engine, session: &mut Session, sql: &str) -> Vec<Vec<String>> {
    match run(engine, session, sql) {
        Outcome::Rows(r) => r
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|v| {
                        if v.is_null() {
                            "NULL".to_owned()
                        } else {
                            v.as_text()
                        }
                    })
                    .collect()
            })
            .collect(),
        Outcome::Affected { .. } => panic!("expected rows from: {sql}"),
    }
}

fn affected(engine: &Engine, session: &mut Session, sql: &str) -> u64 {
    match run(engine, session, sql) {
        Outcome::Affected { rows, .. } => rows,
        Outcome::Rows(_) => panic!("expected an affected count from: {sql}"),
    }
}

fn widgets(engine: &Engine, session: &mut Session) {
    run(
        engine,
        session,
        "CREATE TABLE Widget (
            id VARCHAR(21) NOT NULL,
            name VARCHAR(120) NOT NULL,
            count INTEGER NOT NULL DEFAULT 0,
            note VARCHAR(50) NULL,
            PRIMARY KEY (id)
        )",
    );
    run(
        engine,
        session,
        "INSERT INTO Widget (id, name, count, note) VALUES
            ('a', 'alpha', 3, 'first'),
            ('b', 'beta', 1, NULL),
            ('c', 'gamma', 2, 'third')",
    );
}

#[test]
fn create_insert_and_select() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let all = rows(
        &engine,
        &mut session,
        "SELECT id, count FROM Widget ORDER BY id",
    );
    assert_eq!(all, vec![vec!["a", "3"], vec!["b", "1"], vec!["c", "2"]]);
}

#[test]
fn where_clause_filters() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let filtered = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE count > 1 ORDER BY id",
    );
    assert_eq!(filtered, vec![vec!["a"], vec!["c"]]);
}

/// NULL comparisons are unknown, so a row with NULL matches neither `= x` nor
/// `<> x`. Getting this wrong silently drops rows.
#[test]
fn null_comparison_matches_neither_side() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert!(rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE note = 'first' AND note <> 'first'"
    )
    .is_empty());
    let equal = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE note <> 'first' ORDER BY id",
    );
    assert_eq!(equal, vec![vec!["c"]], "the NULL note must not match <>");

    let is_null = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE note IS NULL",
    );
    assert_eq!(is_null, vec![vec!["b"]]);
}

/// ORDER BY may name a column the SELECT list does not return.
#[test]
fn order_by_a_column_that_is_not_projected() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let ordered = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget ORDER BY count DESC",
    );
    assert_eq!(ordered, vec![vec!["a"], vec!["c"], vec!["b"]]);
}

#[test]
fn limit_and_offset() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let page = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget ORDER BY id LIMIT 2 OFFSET 1",
    );
    assert_eq!(page, vec![vec!["b"], vec!["c"]]);
}

#[test]
fn aggregates_ignore_nulls_but_count_star_does_not() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let agg = rows(
        &engine,
        &mut session,
        "SELECT COUNT(*), COUNT(note), SUM(count), MIN(count), MAX(count) FROM Widget",
    );
    assert_eq!(agg, vec![vec!["3", "2", "6", "1", "3"]]);
}

#[test]
fn update_and_delete_report_affected_rows() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert_eq!(
        affected(
            &engine,
            &mut session,
            "UPDATE Widget SET count = 10 WHERE id = 'a'"
        ),
        1
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT count FROM Widget WHERE id = 'a'"
        ),
        vec![vec!["10"]]
    );

    assert_eq!(
        affected(&engine, &mut session, "DELETE FROM Widget WHERE count < 3"),
        2
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["1"]]
    );
}

#[test]
fn a_duplicate_primary_key_is_rejected() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let err = engine
        .execute(
            &mut session,
            "INSERT INTO Widget (id, name) VALUES ('a', 'again')",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1062, "expected ER_DUP_ENTRY, got: {err}");
}

/// INSERT IGNORE turns a constraint violation into a skipped row.
#[test]
fn insert_ignore_skips_a_duplicate() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert_eq!(
        affected(
            &engine,
            &mut session,
            "INSERT IGNORE INTO Widget (id, name) VALUES ('a', 'again')"
        ),
        0
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["3"]]
    );
}

#[test]
fn a_not_null_column_rejects_null() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let err = engine
        .execute(
            &mut session,
            "INSERT INTO Widget (id, name) VALUES ('z', NULL)",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1048, "expected ER_BAD_NULL_ERROR, got: {err}");
}

#[test]
fn bound_parameters_are_substituted_positionally() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let outcome = engine
        .execute(
            &mut session,
            "SELECT id FROM Widget WHERE count >= ? AND name <> ? ORDER BY id LIMIT ?",
            &[Value::Int(2), Value::text("gamma"), Value::Int(5)],
        )
        .unwrap();

    let Outcome::Rows(result) = outcome else {
        panic!("expected rows")
    };
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0].as_text(), "a");
}

#[test]
fn defaults_apply_when_a_column_is_omitted() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "INSERT INTO Widget (id, name) VALUES ('d', 'delta')",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT count, note FROM Widget WHERE id = 'd'"
        ),
        vec![vec!["0", "NULL"]]
    );
}

/// An expression default is evaluated per row, so it is stored rather than
/// shared from the DDL.
#[test]
fn an_expression_default_produces_a_value() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Stamped (id VARCHAR(4) NOT NULL, at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3), PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Stamped (id) VALUES ('x')",
    );

    let stamped = rows(&engine, &mut session, "SELECT at FROM Stamped");
    assert!(
        stamped[0][0].starts_with("20"),
        "expected a timestamp, got {:?}",
        stamped[0][0]
    );
}

#[test]
fn inner_and_left_joins() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Tag (widgetId VARCHAR(21) NOT NULL, label VARCHAR(20) NOT NULL)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Tag (widgetId, label) VALUES ('a', 'red'), ('a', 'blue')",
    );

    let inner = rows(
        &engine,
        &mut session,
        "SELECT Widget.id, Tag.label FROM Widget INNER JOIN Tag ON Tag.widgetId = Widget.id ORDER BY Tag.label",
    );
    assert_eq!(inner, vec![vec!["a", "blue"], vec!["a", "red"]]);

    // A LEFT JOIN keeps rows with no match, padded with NULL.
    let left = rows(
        &engine,
        &mut session,
        "SELECT Widget.id, Tag.label FROM Widget LEFT JOIN Tag ON Tag.widgetId = Widget.id ORDER BY Widget.id, Tag.label",
    );
    assert_eq!(
        left,
        vec![
            vec!["a", "blue"],
            vec!["a", "red"],
            vec!["b", "NULL"],
            vec!["c", "NULL"],
        ]
    );
}

#[test]
fn information_schema_reports_tables_and_columns() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let tables = rows(
        &engine,
        &mut session,
        "SELECT table_name FROM information_schema.tables WHERE table_schema = 'testdb'",
    );
    assert_eq!(tables, vec![vec!["Widget"]]);

    let columns = rows(
        &engine,
        &mut session,
        "SELECT column_name, data_type, is_nullable FROM information_schema.columns
         WHERE table_schema = 'testdb' ORDER BY ordinal_position",
    );
    assert_eq!(
        columns,
        vec![
            vec!["id", "varchar", "NO"],
            vec!["name", "varchar", "NO"],
            vec!["count", "int", "NO"],
            vec!["note", "varchar", "YES"],
        ]
    );
}

/// Prisma's migration engine joins information_schema tables to each other,
/// which is what forced joins into the prototype.
#[test]
fn information_schema_tables_can_be_joined() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let joined = rows(
        &engine,
        &mut session,
        "SELECT DISTINCT t.table_name FROM information_schema.tables AS t
         JOIN information_schema.columns AS c ON c.table_name = t.table_name
         WHERE t.table_schema = 'testdb' AND t.table_type = 'BASE TABLE'",
    );
    assert_eq!(joined, vec![vec!["Widget"]]);
}

#[test]
fn information_schema_reports_the_primary_key() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let constraints = rows(
        &engine,
        &mut session,
        "SELECT constraint_name, constraint_type FROM information_schema.table_constraints
         WHERE table_schema = 'testdb'",
    );
    assert_eq!(constraints, vec![vec!["PRIMARY", "PRIMARY KEY"]]);
}

#[test]
fn alter_table_adds_and_drops_columns() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "ALTER TABLE Widget ADD COLUMN extra VARCHAR(10) NULL",
    );
    // Existing rows gain the new column rather than becoming ragged.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT extra FROM Widget WHERE id = 'a'"
        ),
        vec![vec!["NULL"]]
    );

    run(
        &engine,
        &mut session,
        "ALTER TABLE Widget DROP COLUMN extra",
    );
    assert!(engine
        .execute(&mut session, "SELECT extra FROM Widget", &[])
        .is_err());
}

#[test]
fn user_variables_persist_across_statements() {
    let (engine, mut session) = engine();

    run(&engine, &mut session, "SET @counter = 41");
    assert_eq!(
        rows(&engine, &mut session, "SELECT @counter"),
        vec![vec!["41"]]
    );
}

#[test]
fn the_connection_preamble_returns_a_row() {
    let (engine, mut session) = engine();

    let preamble = rows(
        &engine,
        &mut session,
        "SELECT @@socket, @@max_allowed_packet, @@wait_timeout",
    );
    assert_eq!(
        preamble.len(),
        1,
        "an empty result here panics Prisma's driver"
    );
    assert_eq!(preamble[0].len(), 3);
}

#[test]
fn an_unknown_system_variable_errors_rather_than_returning_nothing() {
    let (engine, mut session) = engine();
    let err = engine
        .execute(&mut session, "SELECT @@definitely_not_real", &[])
        .unwrap_err();
    assert_eq!(err.code, 1193);
}

/// Unimplemented features must say so rather than return a plausible answer.
#[test]
fn unsupported_features_error_rather_than_guess() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    for sql in [
        "SELECT count FROM Widget GROUP BY count",
        "CREATE TABLE T (a INT, CHECK (a > 0))",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1235, "expected ER_NOT_SUPPORTED_YET for: {sql}");
    }
}

#[test]
fn a_missing_table_reports_the_mysql_error_code() {
    let (engine, mut session) = engine();
    let err = engine
        .execute(&mut session, "SELECT * FROM Nope", &[])
        .unwrap_err();
    assert_eq!(err.code, 1146);
    assert_eq!(err.sql_state, "42S02");
}

#[test]
fn use_switches_the_current_database() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE DATABASE other");
    run(&engine, &mut session, "USE other");
    assert_eq!(session.current_database.as_deref(), Some("other"));

    // Tables are per-database, so the original one is not visible here.
    assert!(engine
        .execute(&mut session, "SELECT * FROM Widget", &[])
        .is_err());
}

#[test]
fn qualified_names_reach_across_databases() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(&engine, &mut session, "CREATE DATABASE other");
    run(&engine, &mut session, "USE other");

    let across = rows(
        &engine,
        &mut session,
        "SELECT id FROM testdb.Widget ORDER BY id LIMIT 1",
    );
    assert_eq!(across, vec![vec!["a"]]);
}

// ---------------------------------------------------------------------------
// Regression tests for issues found in code review on 2026-08-18.
// ---------------------------------------------------------------------------

/// The rename target's Display renders "TO Gadget", so using it verbatim named
/// the table "TO Gadget".
#[test]
fn rename_table_uses_the_name_not_the_rendered_clause() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(&engine, &mut session, "ALTER TABLE Widget RENAME TO Gadget");

    let tables = rows(
        &engine,
        &mut session,
        "SELECT table_name FROM information_schema.tables WHERE table_schema = 'testdb'",
    );
    assert_eq!(tables, vec![vec!["Gadget"]]);
}

/// Removing by the name the user typed rather than the stored key left the
/// rows present under both names.
#[test]
fn rename_table_removes_the_original_even_when_the_case_differs() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(&engine, &mut session, "ALTER TABLE widget RENAME TO Gadget");

    let tables = rows(
        &engine,
        &mut session,
        "SELECT table_name FROM information_schema.tables WHERE table_schema = 'testdb'",
    );
    assert_eq!(
        tables.len(),
        1,
        "the original table must not survive: {tables:?}"
    );
}

/// A column-level UNIQUE reported COLUMN_KEY = 'UNI' while admitting
/// duplicates, which is a constraint that exists only in the metadata.
#[test]
fn a_column_level_unique_is_enforced() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Person (id INT NOT NULL, email VARCHAR(50) UNIQUE, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Person VALUES (1, 'a@b.com')",
    );

    let err = engine
        .execute(
            &mut session,
            "INSERT INTO Person VALUES (2, 'a@b.com')",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1062, "expected ER_DUP_ENTRY, got: {err}");
}

/// Uniqueness follows the same case-insensitive collation the server reports.
#[test]
fn uniqueness_uses_the_advertised_collation() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Person (id INT NOT NULL, email VARCHAR(50) UNIQUE, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Person VALUES (1, 'a@b.com')",
    );

    let err = engine
        .execute(
            &mut session,
            "INSERT INTO Person VALUES (2, 'A@B.COM')",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1062);
}

#[test]
fn a_where_clause_matches_case_insensitively() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let found = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE name = 'ALPHA'",
    );
    assert_eq!(found, vec![vec!["a"]]);
}

/// A VALUES row of the wrong width was truncated or padded, storing corrupt
/// data for a client bug that MySQL would have rejected.
#[test]
fn a_values_row_of_the_wrong_width_is_rejected() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (a INT, b INT, c INT)",
    );

    for sql in [
        "INSERT INTO T (a, b, c) VALUES (1, 2)",
        "INSERT INTO T (a, b) VALUES (1, 2, 3)",
        "INSERT INTO T (a, b, c) VALUES (1, 2, 3), (4, 5)",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1136, "expected a column count error for: {sql}");
    }
}

/// An explicit id must move the counter past it, or a seeded fixture collides
/// with generated ids later.
#[test]
fn an_explicit_value_advances_the_auto_increment_counter() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL AUTO_INCREMENT, v INT, PRIMARY KEY (id))",
    );

    run(&engine, &mut session, "INSERT INTO T (id, v) VALUES (5, 1)");
    run(&engine, &mut session, "INSERT INTO T (v) VALUES (2)");

    let ids = rows(&engine, &mut session, "SELECT id FROM T ORDER BY id");
    assert_eq!(ids, vec![vec!["5"], vec!["6"]]);
}

/// MySQL treats a backslash as LIKE's escape character without an ESCAPE
/// clause. ORMs rely on that when escaping user input.
#[test]
fn like_treats_backslash_as_the_default_escape() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE T (a VARCHAR(10))");
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES ('a_b'), ('axb')",
    );

    let matched = rows(
        &engine,
        &mut session,
        r"SELECT a FROM T WHERE a LIKE 'a\_b'",
    );
    assert_eq!(matched, vec![vec!["a_b"]], "the underscore must be literal");
}

/// A NOT NULL column added to a populated table must not back-fill NULL into
/// rows the metadata says cannot hold one.
#[test]
fn adding_a_not_null_column_backfills_a_usable_default() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "ALTER TABLE Widget ADD COLUMN rank INTEGER NOT NULL",
    );

    let filled = rows(
        &engine,
        &mut session,
        "SELECT rank FROM Widget WHERE id = 'a'",
    );
    assert_eq!(filled, vec![vec!["0"]]);
    assert!(rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE rank IS NULL"
    )
    .is_empty());
}

/// Registering a unique index over data that already violates it leaves the
/// table permanently inconsistent with its own constraint.
#[test]
fn create_unique_index_rejects_existing_duplicates() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE T (a INT)");
    run(&engine, &mut session, "INSERT INTO T VALUES (1), (1)");

    let err = engine
        .execute(&mut session, "CREATE UNIQUE INDEX t_a ON T (a)", &[])
        .unwrap_err();
    assert_eq!(err.code, 1062);

    // A non-unique index over the same data is fine.
    run(&engine, &mut session, "CREATE INDEX t_a_plain ON T (a)");
}

/// information_schema is listed by SHOW DATABASES and served when qualified,
/// so a client that enumerates databases and switches into each must not fail.
#[test]
fn use_can_select_information_schema() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(&engine, &mut session, "USE information_schema");
    assert_eq!(
        session.current_database.as_deref(),
        Some("information_schema")
    );

    let unqualified = rows(
        &engine,
        &mut session,
        "SELECT table_name FROM tables WHERE table_schema = 'testdb'",
    );
    assert_eq!(unqualified, vec![vec!["Widget"]]);
}

/// The binder and the placeholder counter must agree on comment syntax, or
/// parameters bind one position off.
#[test]
fn a_hash_comment_does_not_shift_parameters() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let outcome = engine
        .execute(
            &mut session,
            "SELECT id FROM Widget WHERE count = ? # filter ?\n  AND name = ?",
            &[Value::Int(3), Value::text("alpha")],
        )
        .unwrap();

    let Outcome::Rows(result) = outcome else {
        panic!("expected rows")
    };
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0].as_text(), "a");
}
