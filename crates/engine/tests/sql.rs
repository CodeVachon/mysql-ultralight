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

// ---------------------------------------------------------------------------
// Views, dynamic SQL, and sql_mode. Motte #0018 through #0021.
// ---------------------------------------------------------------------------

#[test]
fn a_view_is_queryable_like_a_table() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "CREATE VIEW BusyWidget AS SELECT id, count FROM Widget WHERE count > 1",
    );

    let through_view = rows(
        &engine,
        &mut session,
        "SELECT id FROM BusyWidget ORDER BY id",
    );
    assert_eq!(through_view, vec![vec!["a"], vec!["c"]]);

    // A view reflects later writes, because it is a query and not a copy.
    run(
        &engine,
        &mut session,
        "UPDATE Widget SET count = 0 WHERE id = 'a'",
    );
    let after = rows(
        &engine,
        &mut session,
        "SELECT id FROM BusyWidget ORDER BY id",
    );
    assert_eq!(after, vec![vec!["c"]]);
}

#[test]
fn a_view_can_be_filtered_and_joined() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE VIEW W AS SELECT id, name, count FROM Widget",
    );

    let filtered = rows(&engine, &mut session, "SELECT name FROM W WHERE count = 1");
    assert_eq!(filtered, vec![vec!["beta"]]);

    let joined = rows(
        &engine,
        &mut session,
        "SELECT W.name FROM W JOIN Widget ON Widget.id = W.id WHERE W.id = 'c'",
    );
    assert_eq!(joined, vec![vec!["gamma"]]);
}

#[test]
fn create_or_replace_view_redefines_it() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "CREATE VIEW V AS SELECT id FROM Widget WHERE id = 'a'",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM V"),
        vec![vec!["1"]]
    );

    run(
        &engine,
        &mut session,
        "CREATE OR REPLACE VIEW V AS SELECT id FROM Widget",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM V"),
        vec![vec!["3"]]
    );

    // Without OR REPLACE, redefining is an error.
    assert!(engine
        .execute(&mut session, "CREATE VIEW V AS SELECT id FROM Widget", &[])
        .is_err());

    run(&engine, &mut session, "DROP VIEW V");
    assert!(engine
        .execute(&mut session, "SELECT * FROM V", &[])
        .is_err());
}

#[test]
fn information_schema_reports_views() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE VIEW V AS SELECT id FROM Widget",
    );

    let listed = rows(
        &engine,
        &mut session,
        "SELECT table_name FROM information_schema.views WHERE table_schema = 'testdb'",
    );
    assert_eq!(listed, vec![vec!["V"]]);

    // Prisma's first introspection step filters tables by type, so a view must
    // appear there as a VIEW rather than a BASE TABLE.
    let base = rows(
        &engine,
        &mut session,
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = 'testdb' AND table_type = 'BASE TABLE'",
    );
    assert_eq!(base, vec![vec!["Widget"]]);
}

/// The idiom the migrations use for conditional DDL: read a count into a user
/// variable, branch with IF(), then prepare and execute the built statement.
#[test]
fn the_prepare_execute_idiom_runs() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "SET @exists := (SELECT COUNT(*) FROM Widget WHERE id = 'z')",
    );
    // A quote inside a MySQL string literal is doubled, so the built statement
    // survives being carried through a variable as text.
    run(
        &engine,
        &mut session,
        "SET @sql = IF(@exists = 0,          'INSERT INTO Widget (id, name) VALUES (''z'', ''zeta'')', 'SELECT 1')",
    );
    run(&engine, &mut session, "PREPARE stmt FROM @sql");
    run(&engine, &mut session, "EXECUTE stmt");
    run(&engine, &mut session, "DEALLOCATE PREPARE stmt");

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT name FROM Widget WHERE id = 'z'"
        ),
        vec![vec!["zeta"]]
    );
}

#[test]
fn execute_binds_its_using_list() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "SET @q = 'SELECT id FROM Widget WHERE count = ?'",
    );
    run(&engine, &mut session, "PREPARE find FROM @q");
    run(&engine, &mut session, "SET @wanted = 2");

    assert_eq!(
        rows(&engine, &mut session, "EXECUTE find USING @wanted"),
        vec![vec!["c"]]
    );
}

#[test]
fn executing_an_unknown_statement_is_an_error() {
    let (engine, mut session) = engine();
    let err = engine
        .execute(&mut session, "EXECUTE nope", &[])
        .unwrap_err();
    assert_eq!(err.code, 1243);
}

#[test]
fn colon_equals_assigns_a_user_variable() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "SET @x := 7");
    assert_eq!(rows(&engine, &mut session, "SELECT @x"), vec![vec!["7"]]);
}

#[test]
fn rename_index_and_drop_event_are_accepted() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (a INT NOT NULL, b INT, PRIMARY KEY (a))",
    );
    run(&engine, &mut session, "CREATE INDEX old_name ON T (b)");
    run(
        &engine,
        &mut session,
        "ALTER TABLE T RENAME INDEX old_name TO new_name",
    );

    let indexes = rows(
        &engine,
        &mut session,
        "SELECT DISTINCT index_name FROM information_schema.statistics
         WHERE table_schema = 'testdb' ORDER BY index_name",
    );
    // Ordering follows the case-insensitive collation the server advertises,
    // so "new_name" sorts before "PRIMARY".
    assert_eq!(indexes, vec![vec!["new_name"], vec!["PRIMARY"]]);

    // No CREATE EVENT exists in the target codebase, so the drop is a no-op.
    run(&engine, &mut session, "DROP EVENT IF EXISTS `Legacy`");
}

#[test]
fn renaming_an_index_that_does_not_exist_is_an_error() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    let err = engine
        .execute(
            &mut session,
            "ALTER TABLE Widget RENAME INDEX nope TO other",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1176);
}

/// `||` means OR by default and concatenation under PIPES_AS_CONCAT. The same
/// text has to produce different answers depending on the mode.
#[test]
fn pipes_follow_sql_mode() {
    let (engine, mut session) = engine();

    let as_or = rows(&engine, &mut session, "SELECT 1 || 0");
    assert_eq!(as_or, vec![vec!["1"]], "by default `||` is logical OR");

    session.config.sql_mode.push_str(",PIPES_AS_CONCAT");
    let as_concat = rows(&engine, &mut session, "SELECT 'a' || 'b'");
    assert_eq!(as_concat, vec![vec!["ab"]]);
}

/// Under ANSI_QUOTES a double-quoted word is a column reference.
#[test]
fn ansi_quotes_changes_what_a_quoted_word_means() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    // Without the mode it is a string literal, so every row returns it.
    let literal = rows(
        &engine,
        &mut session,
        r#"SELECT "name" FROM Widget LIMIT 1"#,
    );
    assert_eq!(literal, vec![vec!["name"]]);

    session.config.sql_mode.push_str(",ANSI_QUOTES");
    let column = rows(
        &engine,
        &mut session,
        r#"SELECT "name" FROM Widget WHERE id = 'a'"#,
    );
    assert_eq!(
        column,
        vec![vec!["alpha"]],
        "with ANSI_QUOTES it is the column"
    );
}

/// `LEFT (` is the LEFT() function under IGNORE_SPACE, not a join.
#[test]
fn ignore_space_allows_a_gap_before_a_call() {
    let (engine, mut session) = engine();
    session.config.sql_mode.push_str(",IGNORE_SPACE");

    assert_eq!(
        rows(&engine, &mut session, "SELECT LEFT ('abcdef', 3)"),
        vec![vec!["abc"]]
    );
}

/// Scalar subqueries, which the migrations reach for constantly to test
/// information_schema before altering anything.
#[test]
fn a_scalar_subquery_evaluates_to_one_value() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT (SELECT COUNT(*) FROM Widget)"
        ),
        vec![vec!["3"]]
    );
    // No rows means NULL, not an error.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT (SELECT id FROM Widget WHERE id = 'zz')"
        ),
        vec![vec!["NULL"]]
    );
}

/// A scalar subquery returning several rows is an error, not a silent pick.
#[test]
fn a_scalar_subquery_returning_many_rows_is_an_error() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let err = engine
        .execute(&mut session, "SELECT (SELECT id FROM Widget)", &[])
        .unwrap_err();
    assert_eq!(err.code, 1242);
}

#[test]
fn exists_and_in_subqueries_filter() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Tag (widgetId VARCHAR(21) NOT NULL)",
    );
    run(&engine, &mut session, "INSERT INTO Tag VALUES ('a'), ('c')");

    let in_sub = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE id IN (SELECT widgetId FROM Tag) ORDER BY id",
    );
    assert_eq!(in_sub, vec![vec!["a"], vec!["c"]]);

    let not_in = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE id NOT IN (SELECT widgetId FROM Tag)",
    );
    assert_eq!(not_in, vec![vec!["b"]]);

    let exists = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE EXISTS (SELECT 1 FROM Tag WHERE Tag.widgetId = Widget.id) ORDER BY id",
    );
    assert_eq!(exists, vec![vec!["a"], vec!["c"]]);
}

/// A subquery that cannot be evaluated must surface, not become NULL. SET was
/// swallowing the error, so `@x` silently held NULL and every later branch on
/// it took the wrong path.
#[test]
fn set_reports_an_evaluation_error_instead_of_storing_null() {
    let (engine, mut session) = engine();
    let err = engine
        .execute(&mut session, "SET @x = (SELECT id FROM NoSuchTable)", &[])
        .unwrap_err();
    assert_eq!(err.code, 1146);
    assert_eq!(
        session.user_variable("x"),
        Value::Null,
        "nothing was stored"
    );
}

/// The conditional-DDL shape the migrations use: branch on a count read from
/// information_schema, then build and run the statement.
#[test]
fn conditional_ddl_against_information_schema() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "SET @has_column := (SELECT COUNT(*) FROM information_schema.columns
                             WHERE table_schema = 'testdb' AND table_name = 'Widget'
                               AND column_name = 'extra')",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT @has_column"),
        vec![vec!["0"]]
    );

    run(
        &engine,
        &mut session,
        "SET @sql = IF(@has_column = 0, 'ALTER TABLE Widget ADD COLUMN extra INT NULL', 'SELECT 1')",
    );
    run(&engine, &mut session, "PREPARE stmt FROM @sql");
    run(&engine, &mut session, "EXECUTE stmt");
    run(&engine, &mut session, "DEALLOCATE PREPARE stmt");

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT extra FROM Widget WHERE id = 'a'"
        ),
        vec![vec!["NULL"]]
    );
}

/// A view's columns must carry the types the definition produces, and appear
/// in information_schema and SHOW. Prisma finds a view in TABLES and then reads
/// COLUMNS for it, so a zero-column answer reads as a broken object.
#[test]
fn a_view_reports_its_columns_and_their_types() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE VIEW V AS SELECT id, count FROM Widget",
    );

    let columns = rows(
        &engine,
        &mut session,
        "SELECT column_name, data_type FROM information_schema.columns
         WHERE table_schema = 'testdb' AND table_name = 'V' ORDER BY ordinal_position",
    );
    assert_eq!(
        columns,
        vec![vec!["id", "varchar"], vec!["count", "bigint"]]
    );

    let shown = rows(&engine, &mut session, "SHOW COLUMNS FROM V");
    assert_eq!(shown.len(), 2);

    // SHOW TABLES lists views, as MySQL does and as TABLES already reported.
    let tables = rows(&engine, &mut session, "SHOW TABLES");
    assert_eq!(tables, vec![vec!["V"], vec!["Widget"]]);
}

#[test]
fn explicit_view_column_names_are_used() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE VIEW V (key_id, how_many) AS SELECT id, count FROM Widget",
    );

    let columns = rows(
        &engine,
        &mut session,
        "SELECT column_name FROM information_schema.columns
         WHERE table_name = 'V' ORDER BY ordinal_position",
    );
    assert_eq!(columns, vec![vec!["key_id"], vec!["how_many"]]);
}

/// A view resolves before a table, so letting a table take a view's name makes
/// the table permanently unreachable.
#[test]
fn a_table_cannot_take_a_views_name() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE VIEW V AS SELECT id FROM Widget",
    );

    let err = engine
        .execute(&mut session, "CREATE TABLE V (z INT)", &[])
        .unwrap_err();
    assert_eq!(err.code, 1050);
}

/// A correlated subquery with no FROM of its own must still see the outer row.
#[test]
fn a_correlated_subquery_without_from_sees_the_outer_row() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let echoed = rows(
        &engine,
        &mut session,
        "SELECT (SELECT id) FROM Widget ORDER BY id LIMIT 1",
    );
    assert_eq!(echoed, vec![vec!["a"]]);
}

/// Under ANSI_QUOTES a qualified name arrives double-quoted per part.
#[test]
fn an_ansi_quoted_qualified_name_resolves() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    session.config.sql_mode.push_str(",ANSI_QUOTES");

    let found = rows(
        &engine,
        &mut session,
        r#"SELECT id FROM "testdb"."Widget" ORDER BY id LIMIT 1"#,
    );
    assert_eq!(found, vec![vec!["a"]]);
}

/// A multi-action ALTER must not have its rename applied while the rest is
/// silently dropped.
#[test]
fn a_multi_action_alter_is_not_partially_applied() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (a INT NOT NULL, b INT, PRIMARY KEY (a))",
    );
    run(&engine, &mut session, "CREATE INDEX old_ix ON T (b)");

    let outcome = engine.execute(
        &mut session,
        "ALTER TABLE T RENAME INDEX old_ix TO new_ix, ADD COLUMN c INT",
        &[],
    );

    match outcome {
        // Either the whole statement applies, or it is refused. What must not
        // happen is a success that dropped the ADD COLUMN.
        Ok(_) => {
            let columns = rows(&engine, &mut session, "SELECT c FROM T");
            assert!(columns.is_empty() || columns[0].len() == 1);
        }
        Err(e) => assert_ne!(e.code, 0),
    }
}
