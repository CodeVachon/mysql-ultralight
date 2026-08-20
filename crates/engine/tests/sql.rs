//! End-to-end SQL tests against the engine, with no socket in the way.

use mysql_ultralight_engine::{Engine, Outcome, ServerConfig, Session, Value};

/// A fresh engine with one database selected.
///
/// Aliased so a test that needs a *second* engine can still reach it after
/// binding a local named `engine`.
fn fresh_engine() -> (std::sync::Arc<Engine>, Session) {
    engine()
}

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

    // The list shrinks as features land; GROUP BY, window functions, CHECK
    // constraints and INTERSECT all used to be here. Named windows remain.
    let sql = "SELECT id FROM Widget WINDOW w AS (ORDER BY id)";
    let err = engine.execute(&mut session, sql, &[]).unwrap_err();
    assert_eq!(err.code, 1235, "expected ER_NOT_SUPPORTED_YET for: {sql}");
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

/// Scalar executor, which the migrations reach for constantly to test
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
fn exists_and_in_executor_filter() {
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
    // `count` is an INTEGER and says so. It read back as `bigint` while a
    // view's shape was rebuilt from the wire type, which widens every integer
    // size to one.
    assert_eq!(columns, vec![vec!["id", "varchar"], vec!["count", "int"]]);

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

// ---------------------------------------------------------------------------
// GROUP BY, UNION and window functions. Motte #0024.
// ---------------------------------------------------------------------------

fn sales(engine: &Engine, session: &mut Session) {
    run(
        engine,
        session,
        "CREATE TABLE Sale (
            id INT NOT NULL,
            region VARCHAR(10) NOT NULL,
            rep VARCHAR(10) NOT NULL,
            amount INT NOT NULL,
            note VARCHAR(10) NULL,
            PRIMARY KEY (id)
        )",
    );
    run(
        engine,
        session,
        "INSERT INTO Sale (id, region, rep, amount, note) VALUES
            (1, 'north', 'ann', 100, 'x'),
            (2, 'north', 'bob', 200, NULL),
            (3, 'south', 'ann', 50,  'y'),
            (4, 'south', 'cid', 75,  NULL),
            (5, 'south', 'cid', 25,  'z')",
    );
}

#[test]
fn group_by_aggregates_per_group() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let by_region = rows(
        &engine,
        &mut session,
        "SELECT region, COUNT(*), SUM(amount) FROM Sale GROUP BY region ORDER BY region",
    );
    assert_eq!(
        by_region,
        vec![vec!["north", "2", "300"], vec!["south", "3", "150"]]
    );
}

#[test]
fn group_by_several_columns() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let grouped = rows(
        &engine,
        &mut session,
        "SELECT region, rep, SUM(amount) FROM Sale GROUP BY region, rep ORDER BY region, rep",
    );
    assert_eq!(
        grouped,
        vec![
            vec!["north", "ann", "100"],
            vec!["north", "bob", "200"],
            vec!["south", "ann", "50"],
            vec!["south", "cid", "100"],
        ]
    );
}

/// HAVING filters groups after aggregation, unlike WHERE.
#[test]
fn having_filters_groups() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let big = rows(
        &engine,
        &mut session,
        "SELECT region FROM Sale GROUP BY region HAVING SUM(amount) > 200",
    );
    assert_eq!(big, vec![vec!["north"]]);

    let many = rows(
        &engine,
        &mut session,
        "SELECT region FROM Sale GROUP BY region HAVING COUNT(*) >= 3",
    );
    assert_eq!(many, vec![vec!["south"]]);
}

/// An aggregate inside a larger expression must fold before the surrounding
/// arithmetic runs.
#[test]
fn an_aggregate_can_appear_inside_an_expression() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let averaged = rows(
        &engine,
        &mut session,
        "SELECT region, SUM(amount) / COUNT(*) FROM Sale GROUP BY region ORDER BY region",
    );
    assert_eq!(averaged, vec![vec!["north", "150"], vec!["south", "50"]]);
}

/// Grouping treats NULLs as one group, unlike comparison, which treats them as
/// unknown and never equal.
#[test]
fn nulls_group_together() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let counted = rows(
        &engine,
        &mut session,
        "SELECT COUNT(*) FROM Sale GROUP BY note ORDER BY COUNT(*) DESC",
    );
    assert_eq!(counted, vec![vec!["2"], vec!["1"], vec!["1"], vec!["1"]]);
}

/// An aggregate with no GROUP BY is one group over every row, and still
/// produces a row when there are none.
#[test]
fn an_ungrouped_aggregate_over_no_rows_returns_one_row() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*), SUM(amount) FROM Sale WHERE amount > 9999"
        ),
        vec![vec!["0", "NULL"]]
    );
    // With GROUP BY, no rows means no groups and therefore no output.
    assert!(rows(
        &engine,
        &mut session,
        "SELECT region FROM Sale WHERE amount > 9999 GROUP BY region"
    )
    .is_empty());
}

#[test]
fn count_distinct_counts_unique_values() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(DISTINCT rep) FROM Sale"
        ),
        vec![vec!["3"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(rep) FROM Sale"),
        vec![vec!["5"]]
    );
}

#[test]
fn group_concat_joins_a_group() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let joined = rows(
        &engine,
        &mut session,
        "SELECT region, GROUP_CONCAT(rep) FROM Sale GROUP BY region ORDER BY region",
    );
    assert_eq!(
        joined,
        vec![vec!["north", "ann,bob"], vec!["south", "ann,cid,cid"]]
    );
}

#[test]
fn union_combines_and_deduplicates() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let deduped = rows(
        &engine,
        &mut session,
        "SELECT rep FROM Sale WHERE region = 'north'
         UNION
         SELECT rep FROM Sale WHERE region = 'south'
         ORDER BY rep",
    );
    assert_eq!(deduped, vec![vec!["ann"], vec!["bob"], vec!["cid"]]);
}

#[test]
fn union_all_keeps_duplicates() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let all = rows(
        &engine,
        &mut session,
        "SELECT rep FROM Sale WHERE region = 'north'
         UNION ALL
         SELECT rep FROM Sale WHERE region = 'south'
         ORDER BY rep",
    );
    assert_eq!(all.len(), 5);
}

#[test]
fn a_union_with_mismatched_column_counts_is_rejected() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let err = engine
        .execute(
            &mut session,
            "SELECT rep FROM Sale UNION SELECT rep, amount FROM Sale",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1222);
}

/// ROW_NUMBER over a partition, which is what the migrations use to
/// deduplicate.
#[test]
fn row_number_numbers_within_a_partition() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let numbered = rows(
        &engine,
        &mut session,
        "SELECT region, amount, ROW_NUMBER() OVER (PARTITION BY region ORDER BY amount DESC) AS rn
         FROM Sale ORDER BY region, rn",
    );
    assert_eq!(
        numbered,
        vec![
            vec!["north", "200", "1"],
            vec!["north", "100", "2"],
            vec!["south", "75", "1"],
            vec!["south", "50", "2"],
            vec!["south", "25", "3"],
        ]
    );
}

/// A window function keeps every row, unlike an aggregate.
#[test]
fn a_window_function_does_not_collapse_the_result() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let windowed = rows(
        &engine,
        &mut session,
        "SELECT id, SUM(amount) OVER (PARTITION BY region) AS region_total
         FROM Sale ORDER BY id",
    );
    assert_eq!(
        windowed,
        vec![
            vec!["1", "300"],
            vec!["2", "300"],
            vec!["3", "150"],
            vec!["4", "150"],
            vec!["5", "150"],
        ]
    );
}

/// RANK leaves a gap after a tie; DENSE_RANK does not.
#[test]
fn rank_and_dense_rank_differ_on_ties() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE S (id INT NOT NULL, v INT, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES (1, 10), (2, 10), (3, 20)",
    );

    let ranked = rows(
        &engine,
        &mut session,
        "SELECT id, RANK() OVER (ORDER BY v), DENSE_RANK() OVER (ORDER BY v) FROM S ORDER BY id",
    );
    assert_eq!(
        ranked,
        vec![
            vec!["1", "1", "1"],
            vec!["2", "1", "1"],
            vec!["3", "3", "2"]
        ]
    );
}

/// A window with no PARTITION BY spans the whole result.
#[test]
fn a_window_without_partition_covers_everything() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let numbered = rows(
        &engine,
        &mut session,
        "SELECT ROW_NUMBER() OVER (ORDER BY amount) FROM Sale",
    );
    assert_eq!(numbered.len(), 5);
    let mut seen: Vec<String> = numbered.into_iter().map(|r| r[0].clone()).collect();
    seen.sort();
    assert_eq!(seen, vec!["1", "2", "3", "4", "5"]);
}

/// The deduplication shape the migrations actually use: number the rows in each
/// partition, then keep the first.
#[test]
fn the_deduplication_idiom_from_the_migrations() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let first_per_region = rows(
        &engine,
        &mut session,
        "SELECT region, rep FROM (
            SELECT region, rep,
                   ROW_NUMBER() OVER (PARTITION BY region ORDER BY amount DESC) AS rn
            FROM Sale
         ) ranked
         WHERE rn = 1
         ORDER BY region",
    );
    assert_eq!(
        first_per_region,
        vec![vec!["north", "bob"], vec!["south", "cid"]]
    );
}

#[test]
fn an_unsupported_window_function_errors() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    // A window frame is still refused; the function itself is supported now.
    let err = engine
        .execute(
            &mut session,
            "SELECT SUM(amount) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) \
             FROM Sale",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1235);
}

// ---------------------------------------------------------------------------
// Regression tests for the grouping review, 2026-08-18.
// ---------------------------------------------------------------------------

/// An aggregate nested in an expression is still an aggregate query. Matching
/// only a bare `COUNT(*)` sent these down the per-row path.
#[test]
fn an_aggregate_nested_in_an_expression_still_groups() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) + 0 FROM Sale"),
        vec![vec!["5"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT SUM(amount) / COUNT(*) FROM Sale"
        ),
        vec![vec!["90"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT ROUND(AVG(amount), 1) FROM Sale"
        ),
        vec![vec!["90"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CASE WHEN COUNT(*) > 1 THEN 'many' ELSE 'one' END FROM Sale"
        ),
        vec![vec!["many"]]
    );
}

/// The silent version of the same bug: an aggregate over no matching rows
/// returned zero rows instead of one row holding the default.
#[test]
fn a_wrapped_aggregate_over_no_rows_still_returns_one_row() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT IFNULL(SUM(amount), 0) FROM Sale WHERE id > 99"
        ),
        vec![vec!["0"]]
    );
}

/// `GROUP BY 1` means the first select column. Read as a literal it gives every
/// row the same key and collapses the table into one group.
#[test]
fn group_by_an_ordinal_names_a_column() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let by_ordinal = rows(
        &engine,
        &mut session,
        "SELECT region, COUNT(*) FROM Sale GROUP BY 1 ORDER BY region",
    );
    assert_eq!(by_ordinal, vec![vec!["north", "2"], vec!["south", "3"]]);
}

/// `ORDER BY 2` names the second output column, in both the grouped and the
/// set-operation paths.
#[test]
fn order_by_an_ordinal_sorts_by_that_column() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let grouped = rows(
        &engine,
        &mut session,
        "SELECT region, SUM(amount) FROM Sale GROUP BY region ORDER BY 2 DESC",
    );
    assert_eq!(grouped, vec![vec!["north", "300"], vec!["south", "150"]]);

    let unioned = rows(
        &engine,
        &mut session,
        "SELECT region FROM Sale WHERE amount > 90
         UNION SELECT region FROM Sale WHERE amount < 60
         ORDER BY 1 DESC",
    );
    assert_eq!(unioned, vec![vec!["south"], vec!["north"]]);
}

/// HAVING may name a select-list alias, which reporting queries rely on.
#[test]
fn having_can_reference_an_alias() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let filtered = rows(
        &engine,
        &mut session,
        "SELECT region, COUNT(*) AS n FROM Sale GROUP BY region HAVING n > 2",
    );
    assert_eq!(filtered, vec![vec!["south", "3"]]);
}

/// A HAVING with no aggregate must still be applied, not silently dropped.
#[test]
fn a_having_without_an_aggregate_is_applied() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let filtered = rows(
        &engine,
        &mut session,
        "SELECT region FROM Sale GROUP BY region HAVING region = 'north'",
    );
    assert_eq!(filtered, vec![vec!["north"]]);
}

/// Under ONLY_FULL_GROUP_BY, selecting a column that is neither grouped nor
/// aggregated is an error. Evaluating it against the group's first row returns
/// an arbitrary value instead.
#[test]
fn only_full_group_by_rejects_an_ungrouped_column() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);
    assert!(session.sql_mode().only_full_group_by);

    let err = engine
        .execute(
            &mut session,
            "SELECT id, COUNT(*) FROM Sale GROUP BY region",
            &[],
        )
        .unwrap_err();
    assert_eq!(
        err.code, 1055,
        "expected ER_WRONG_FIELD_WITH_GROUP, got: {err}"
    );

    // The grouped column itself is fine.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT region, COUNT(*) FROM Sale GROUP BY region ORDER BY region"
        )
        .len(),
        2
    );
}

/// Folding an aggregate must preserve the value, not round-trip it through a
/// string literal: that loses binary data and changes the reported type.
#[test]
fn aggregating_binary_data_preserves_it() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE B (id INT NOT NULL, d VARBINARY(8), PRIMARY KEY (id))",
    );

    engine
        .execute(
            &mut session,
            "INSERT INTO B VALUES (1, ?)",
            &[Value::Bytes(vec![0xc3, 0xff, 0x01])],
        )
        .unwrap();

    let Outcome::Rows(result) = engine
        .execute(&mut session, "SELECT MAX(d) FROM B", &[])
        .unwrap()
    else {
        panic!("expected rows")
    };
    assert_eq!(
        result.rows[0][0],
        Value::Bytes(vec![0xc3, 0xff, 0x01]),
        "the bytes must survive the fold"
    );
}

/// MySQL accepts an IN list whose elements are scalar executor. It is
/// rewritten into a UNION ALL, so this checks the result, not just the parse.
#[test]
fn in_a_list_of_scalar_executor() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Pick (id INT NOT NULL, want VARCHAR(21))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Pick VALUES (1, 'a'), (2, 'c')",
    );

    let picked = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE id IN (
            (SELECT want FROM Pick WHERE id = 1 LIMIT 1),
            (SELECT want FROM Pick WHERE id = 2 LIMIT 1)
         ) ORDER BY id",
    );
    assert_eq!(picked, vec![vec!["a"], vec!["c"]]);

    let excluded = rows(
        &engine,
        &mut session,
        "SELECT id FROM Widget WHERE id NOT IN (
            (SELECT want FROM Pick WHERE id = 1 LIMIT 1),
            (SELECT want FROM Pick WHERE id = 2 LIMIT 1)
         )",
    );
    assert_eq!(excluded, vec![vec!["b"]]);
}

/// An ordinary IN list must keep working; the rewrite only applies to a list
/// whose every element is a parenthesised SELECT.
#[test]
fn an_ordinary_in_list_is_unaffected() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM Widget WHERE id IN ('a', 'c') ORDER BY id"
        ),
        vec![vec!["a"], vec!["c"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM Widget WHERE id IN (SELECT id FROM Widget WHERE count > 2)"
        ),
        vec![vec!["a"]]
    );
}

// ---------------------------------------------------------------------------
// Generated columns and CHECK constraints. Motte #0025.
// ---------------------------------------------------------------------------

#[test]
fn a_generated_column_is_computed_from_the_row() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (
            id INT NOT NULL,
            first VARCHAR(20),
            last VARCHAR(20),
            full_name VARCHAR(60) GENERATED ALWAYS AS (CONCAT(first, ' ', last)),
            PRIMARY KEY (id)
        )",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO T (id, first, last) VALUES (1, 'Ada', 'Lovelace')",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT full_name FROM T"),
        vec![vec!["Ada Lovelace"]]
    );

    // It tracks updates to the columns it reads.
    run(
        &engine,
        &mut session,
        "UPDATE T SET last = 'Byron' WHERE id = 1",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT full_name FROM T"),
        vec![vec!["Ada Byron"]]
    );
}

/// Writing to a generated column is an error, not a silent overwrite.
#[test]
fn a_generated_column_cannot_be_written() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL, a INT, g INT GENERATED ALWAYS AS (a + 1), PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO T (id, a) VALUES (1, 1)");

    for sql in [
        "INSERT INTO T (id, a, g) VALUES (2, 1, 99)",
        "UPDATE T SET g = 99 WHERE id = 1",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(
            err.code, 3105,
            "expected a generated-column error for: {sql}"
        );
    }
}

/// The shape the migrations use: a key that only exists for live rows, so the
/// uniqueness it enforces is conditional. An unevaluated generated column is
/// always NULL, and a NULL never conflicts, so the constraint would not exist.
#[test]
fn a_generated_column_can_carry_a_conditional_unique_key() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Record (
            id INT NOT NULL,
            userId VARCHAR(10) NOT NULL,
            day VARCHAR(10) NOT NULL,
            isDeleted BOOLEAN NOT NULL DEFAULT false,
            activeKey VARCHAR(40) GENERATED ALWAYS AS (
                CASE WHEN isDeleted = false THEN CONCAT(userId, '-', day) END
            ),
            PRIMARY KEY (id),
            UNIQUE KEY active_unique (activeKey)
        )",
    );

    run(
        &engine,
        &mut session,
        "INSERT INTO Record (id, userId, day) VALUES (1, 'u1', 'mon')",
    );

    // A second live row for the same user and day collides.
    let err = engine
        .execute(
            &mut session,
            "INSERT INTO Record (id, userId, day) VALUES (2, 'u1', 'mon')",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1062);

    // A deleted row generates NULL, which never conflicts.
    run(
        &engine,
        &mut session,
        "INSERT INTO Record (id, userId, day, isDeleted) VALUES (3, 'u1', 'mon', true)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Record (id, userId, day, isDeleted) VALUES (4, 'u1', 'mon', true)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Record"),
        vec![vec!["3"]]
    );
}

#[test]
fn a_check_constraint_is_enforced() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL, qty INT, PRIMARY KEY (id), CONSTRAINT positive CHECK (qty > 0))",
    );
    run(&engine, &mut session, "INSERT INTO T VALUES (1, 5)");

    let err = engine
        .execute(&mut session, "INSERT INTO T VALUES (2, -1)", &[])
        .unwrap_err();
    assert_eq!(err.code, 3819, "expected a check violation, got: {err}");

    // UPDATE is held to the same constraint.
    let err = engine
        .execute(&mut session, "UPDATE T SET qty = 0 WHERE id = 1", &[])
        .unwrap_err();
    assert_eq!(err.code, 3819);
}

/// A CHECK passes when it is true or unknown; only an outright false violates.
#[test]
fn a_check_over_null_passes() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL, qty INT, PRIMARY KEY (id), CHECK (qty > 0))",
    );

    run(&engine, &mut session, "INSERT INTO T VALUES (1, NULL)");
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM T"),
        vec![vec!["1"]]
    );
}

#[test]
fn information_schema_reports_a_generated_column() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL, a INT, g INT GENERATED ALWAYS AS (a + 1), PRIMARY KEY (id))",
    );

    let columns = rows(
        &engine,
        &mut session,
        "SELECT column_name FROM information_schema.columns
         WHERE table_name = 'T' ORDER BY ordinal_position",
    );
    assert_eq!(columns, vec![vec!["id"], vec!["a"], vec!["g"]]);
}

// ---------------------------------------------------------------------------
// Foreign key enforcement. Motte #0026.
// ---------------------------------------------------------------------------

/// A parent and a child, with the referential action given by the caller.
fn related(engine: &Engine, session: &mut Session, on_delete: &str) {
    run(
        engine,
        session,
        "CREATE TABLE Parent (id VARCHAR(4) NOT NULL, name VARCHAR(10), PRIMARY KEY (id))",
    );
    run(
        engine,
        session,
        &format!(
            "CREATE TABLE Child (
                id INT NOT NULL,
                parentId VARCHAR(4) NULL,
                PRIMARY KEY (id),
                CONSTRAINT fk_parent FOREIGN KEY (parentId)
                    REFERENCES Parent (id) ON DELETE {on_delete}
            )"
        ),
    );
    run(
        engine,
        session,
        "INSERT INTO Parent VALUES ('p1', 'one'), ('p2', 'two')",
    );
}

#[test]
fn a_child_row_must_reference_an_existing_parent() {
    let (engine, mut session) = engine();
    related(&engine, &mut session, "RESTRICT");

    run(&engine, &mut session, "INSERT INTO Child VALUES (1, 'p1')");

    let err = engine
        .execute(&mut session, "INSERT INTO Child VALUES (2, 'nope')", &[])
        .unwrap_err();
    assert_eq!(
        err.code, 1452,
        "expected a foreign key violation, got: {err}"
    );

    // UPDATE is held to the same constraint.
    let err = engine
        .execute(
            &mut session,
            "UPDATE Child SET parentId = 'nope' WHERE id = 1",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1452);
}

/// A NULL reference points at nothing, so it is exempt.
#[test]
fn a_null_reference_is_allowed() {
    let (engine, mut session) = engine();
    related(&engine, &mut session, "RESTRICT");

    run(&engine, &mut session, "INSERT INTO Child VALUES (1, NULL)");
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Child"),
        vec![vec!["1"]]
    );
}

#[test]
fn on_delete_restrict_blocks_the_parent() {
    let (engine, mut session) = engine();
    related(&engine, &mut session, "RESTRICT");
    run(&engine, &mut session, "INSERT INTO Child VALUES (1, 'p1')");

    let err = engine
        .execute(&mut session, "DELETE FROM Parent WHERE id = 'p1'", &[])
        .unwrap_err();
    assert_eq!(
        err.code, 1451,
        "expected a parent-row violation, got: {err}"
    );

    // A parent with no children still deletes.
    run(&engine, &mut session, "DELETE FROM Parent WHERE id = 'p2'");
}

#[test]
fn on_delete_cascade_removes_the_children() {
    let (engine, mut session) = engine();
    related(&engine, &mut session, "CASCADE");
    run(
        &engine,
        &mut session,
        "INSERT INTO Child VALUES (1, 'p1'), (2, 'p1'), (3, 'p2')",
    );

    run(&engine, &mut session, "DELETE FROM Parent WHERE id = 'p1'");

    let left = rows(&engine, &mut session, "SELECT id FROM Child ORDER BY id");
    assert_eq!(left, vec![vec!["3"]]);
}

#[test]
fn on_delete_set_null_clears_the_reference() {
    let (engine, mut session) = engine();
    related(&engine, &mut session, "SET NULL");
    run(
        &engine,
        &mut session,
        "INSERT INTO Child VALUES (1, 'p1'), (2, 'p2')",
    );

    run(&engine, &mut session, "DELETE FROM Parent WHERE id = 'p1'");

    let refs = rows(
        &engine,
        &mut session,
        "SELECT id, parentId FROM Child ORDER BY id",
    );
    assert_eq!(refs, vec![vec!["1", "NULL"], vec!["2", "p2"]]);
}

/// Cascades chain: deleting a job removes its timesheets, which removes their
/// records. Handling one level would leave orphans behind.
#[test]
fn cascading_deletes_chain_through_several_levels() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE A (id INT NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE B (id INT NOT NULL, aId INT, PRIMARY KEY (id),
         FOREIGN KEY (aId) REFERENCES A (id) ON DELETE CASCADE)",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE C (id INT NOT NULL, bId INT, PRIMARY KEY (id),
         FOREIGN KEY (bId) REFERENCES B (id) ON DELETE CASCADE)",
    );
    run(&engine, &mut session, "INSERT INTO A VALUES (1)");
    run(&engine, &mut session, "INSERT INTO B VALUES (10, 1)");
    run(&engine, &mut session, "INSERT INTO C VALUES (100, 10)");

    run(&engine, &mut session, "DELETE FROM A WHERE id = 1");

    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM B"),
        vec![vec!["0"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM C"),
        vec![vec!["0"]],
        "the grandchild must go too"
    );
}

/// A constraint whose parent table does not exist cannot be checked, and must
/// not fail every write instead.
#[test]
fn a_constraint_against_a_missing_parent_is_inert() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Orphan (id INT NOT NULL, ref INT, PRIMARY KEY (id),
         FOREIGN KEY (ref) REFERENCES NotThere (id))",
    );
    run(&engine, &mut session, "INSERT INTO Orphan VALUES (1, 42)");
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Orphan"),
        vec![vec!["1"]]
    );
}

// ---------------------------------------------------------------------------
// Transactions. Motte #0027.
// ---------------------------------------------------------------------------

#[test]
fn rollback_undoes_the_writes_in_a_transaction() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(&engine, &mut session, "BEGIN");
    run(
        &engine,
        &mut session,
        "INSERT INTO Widget (id, name) VALUES ('z', 'zeta')",
    );
    run(
        &engine,
        &mut session,
        "UPDATE Widget SET count = 99 WHERE id = 'a'",
    );
    run(&engine, &mut session, "DELETE FROM Widget WHERE id = 'b'");
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["3"]]
    );

    run(&engine, &mut session, "ROLLBACK");

    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["3"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT count FROM Widget WHERE id = 'a'"
        ),
        vec![vec!["3"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget ORDER BY id"),
        vec![vec!["a"], vec!["b"], vec!["c"]]
    );
}

#[test]
fn commit_keeps_the_writes() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(&engine, &mut session, "BEGIN");
    run(
        &engine,
        &mut session,
        "INSERT INTO Widget (id, name) VALUES ('z', 'zeta')",
    );
    run(&engine, &mut session, "COMMIT");

    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["4"]]
    );

    // A rollback after the commit has nothing to undo.
    run(&engine, &mut session, "ROLLBACK");
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["4"]]
    );
}

/// DDL is undone too, because the whole catalog is snapshotted. MySQL commits
/// implicitly on DDL instead, so this is more forgiving rather than less.
#[test]
fn rollback_undoes_schema_changes() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(&engine, &mut session, "BEGIN");
    run(
        &engine,
        &mut session,
        "CREATE TABLE Temp (id INT NOT NULL, PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "ROLLBACK");

    assert!(engine
        .execute(&mut session, "SELECT * FROM Temp", &[])
        .is_err());
}

#[test]
fn a_savepoint_rolls_back_only_the_work_after_it() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(&engine, &mut session, "BEGIN");
    run(
        &engine,
        &mut session,
        "INSERT INTO Widget (id, name) VALUES ('d', 'delta')",
    );
    run(&engine, &mut session, "SAVEPOINT here");
    run(
        &engine,
        &mut session,
        "INSERT INTO Widget (id, name) VALUES ('e', 'epsilon')",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["5"]]
    );

    run(&engine, &mut session, "ROLLBACK TO SAVEPOINT here");
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["4"]]
    );

    // The transaction is still open, so the outer rollback still works.
    run(&engine, &mut session, "ROLLBACK");
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["3"]]
    );
}

#[test]
fn rolling_back_to_an_unknown_savepoint_is_an_error() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(&engine, &mut session, "BEGIN");

    let err = engine
        .execute(&mut session, "ROLLBACK TO SAVEPOINT nope", &[])
        .unwrap_err();
    assert_eq!(err.code, 1305);
}

#[test]
fn releasing_a_savepoint_discards_it() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(&engine, &mut session, "BEGIN");
    run(&engine, &mut session, "SAVEPOINT here");
    run(&engine, &mut session, "RELEASE SAVEPOINT here");

    let err = engine
        .execute(&mut session, "ROLLBACK TO SAVEPOINT here", &[])
        .unwrap_err();
    assert_eq!(err.code, 1305);
}

/// A statement outside a transaction commits as it goes.
#[test]
fn autocommit_writes_are_not_undone() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "INSERT INTO Widget (id, name) VALUES ('z', 'zeta')",
    );
    run(&engine, &mut session, "ROLLBACK");

    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["4"]]
    );
}

/// A failed statement inside a transaction leaves its own partial work behind
/// until the transaction is rolled back, as in MySQL.
#[test]
fn a_failed_statement_can_be_rolled_back() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(&engine, &mut session, "BEGIN");
    run(
        &engine,
        &mut session,
        "INSERT INTO Widget (id, name) VALUES ('d', 'delta')",
    );
    assert!(engine
        .execute(
            &mut session,
            "INSERT INTO Widget (id, name) VALUES ('a', 'dup')",
            &[]
        )
        .is_err());

    run(&engine, &mut session, "ROLLBACK");
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["3"]]
    );
}

// ---------------------------------------------------------------------------
// Regression tests for the constraints review, 2026-08-18.
// ---------------------------------------------------------------------------

/// RESTRICT must block the delete, not merely report an error after the parent
/// row is already gone.
#[test]
fn on_delete_restrict_leaves_the_parent_in_place() {
    let (engine, mut session) = engine();
    related(&engine, &mut session, "RESTRICT");
    run(&engine, &mut session, "INSERT INTO Child VALUES (1, 'p1')");

    assert!(engine
        .execute(&mut session, "DELETE FROM Parent WHERE id = 'p1'", &[])
        .is_err());

    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Parent ORDER BY id"),
        vec![vec!["p1"], vec!["p2"]],
        "the parent row must survive a refused delete"
    );
}

/// The cascade cap counts generations. A hub table with many referencing
/// tables is one level deep, however many there are.
#[test]
fn many_sibling_children_are_not_mistaken_for_depth() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Hub (id INT NOT NULL, PRIMARY KEY (id))",
    );
    for n in 0..20 {
        run(
            &engine,
            &mut session,
            &format!(
                "CREATE TABLE Spoke{n} (id INT NOT NULL, hubId INT, PRIMARY KEY (id),
                 FOREIGN KEY (hubId) REFERENCES Hub (id) ON DELETE CASCADE)"
            ),
        );
    }
    run(&engine, &mut session, "INSERT INTO Hub VALUES (1)");
    run(&engine, &mut session, "INSERT INTO Spoke0 VALUES (1, 1)");

    run(&engine, &mut session, "DELETE FROM Hub WHERE id = 1");
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Spoke0"),
        vec![vec!["0"]]
    );
}

/// MySQL checks a foreign key row by row, so a self-referencing multi-row
/// INSERT can point at a row added earlier in the same statement.
#[test]
fn a_self_referencing_multi_row_insert_works() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Node (id INT NOT NULL, parent INT NULL, PRIMARY KEY (id),
         FOREIGN KEY (parent) REFERENCES Node (id))",
    );

    run(
        &engine,
        &mut session,
        "INSERT INTO Node VALUES (1, NULL), (2, 1), (3, 2)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Node"),
        vec![vec!["3"]]
    );
}

/// IGNORE downgrades a foreign key violation to a skipped row, as it does a
/// duplicate key.
#[test]
fn insert_ignore_skips_a_foreign_key_violation() {
    let (engine, mut session) = engine();
    related(&engine, &mut session, "RESTRICT");

    assert_eq!(
        affected(
            &engine,
            &mut session,
            "INSERT IGNORE INTO Child VALUES (1, 'nope')"
        ),
        0
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Child"),
        vec![vec!["0"]]
    );
}

/// SET NULL cannot write into a NOT NULL column; storing one would leave a row
/// the schema says is impossible, which every later UPDATE would then reject.
#[test]
fn set_null_into_a_not_null_column_is_refused() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE P (id INT NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE C (id INT NOT NULL, pid INT NOT NULL, PRIMARY KEY (id),
         FOREIGN KEY (pid) REFERENCES P (id) ON DELETE SET NULL)",
    );
    run(&engine, &mut session, "INSERT INTO P VALUES (1)");
    run(&engine, &mut session, "INSERT INTO C VALUES (1, 1)");

    let err = engine
        .execute(&mut session, "DELETE FROM P WHERE id = 1", &[])
        .unwrap_err();
    assert_eq!(err.code, 1830);
}

/// An UPDATE of a parent key must carry the children with it, or leave them
/// pointing at a value that no longer exists.
#[test]
fn on_update_cascade_follows_the_parent_key() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE P (id VARCHAR(4) NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE C (id INT NOT NULL, pid VARCHAR(4), PRIMARY KEY (id),
         FOREIGN KEY (pid) REFERENCES P (id) ON UPDATE CASCADE)",
    );
    run(&engine, &mut session, "INSERT INTO P VALUES ('p1')");
    run(&engine, &mut session, "INSERT INTO C VALUES (1, 'p1')");

    run(
        &engine,
        &mut session,
        "UPDATE P SET id = 'px' WHERE id = 'p1'",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT pid FROM C"),
        vec![vec!["px"]]
    );
}

#[test]
fn on_update_restrict_blocks_the_parent_key_change() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE P (id VARCHAR(4) NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE C (id INT NOT NULL, pid VARCHAR(4), PRIMARY KEY (id),
         FOREIGN KEY (pid) REFERENCES P (id) ON UPDATE RESTRICT)",
    );
    run(&engine, &mut session, "INSERT INTO P VALUES ('p1')");
    run(&engine, &mut session, "INSERT INTO C VALUES (1, 'p1')");

    let err = engine
        .execute(&mut session, "UPDATE P SET id = 'px' WHERE id = 'p1'", &[])
        .unwrap_err();
    assert_eq!(err.code, 1451);
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM P"),
        vec![vec!["p1"]]
    );
}

/// An inline CHECK is the same constraint written next to the column.
#[test]
fn an_inline_check_is_enforced() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL CHECK (id > 0), PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO T VALUES (1)");

    let err = engine
        .execute(&mut session, "INSERT INTO T VALUES (-1)", &[])
        .unwrap_err();
    assert_eq!(err.code, 3819);
}

/// Whether writing to a generated column is an error must not depend on
/// whether any row happened to match.
#[test]
fn writing_a_generated_column_errors_even_when_no_row_matches() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL, a INT, g INT GENERATED ALWAYS AS (a + 1), PRIMARY KEY (id))",
    );

    let err = engine
        .execute(&mut session, "UPDATE T SET g = 1 WHERE id = 999", &[])
        .unwrap_err();
    assert_eq!(err.code, 3105);
}

/// Both INSERT forms must agree: a value in a generated column's position is
/// an error whether or not a column list was given.
#[test]
fn a_positional_insert_cannot_fill_a_generated_column() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL, a INT, g INT GENERATED ALWAYS AS (a + 1), PRIMARY KEY (id))",
    );

    let err = engine
        .execute(&mut session, "INSERT INTO T VALUES (1, 5, 999)", &[])
        .unwrap_err();
    assert_eq!(err.code, 3105);

    // DEFAULT and NULL are accepted in that position.
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES (2, 5, DEFAULT)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT g FROM T"),
        vec![vec!["6"]]
    );
}

/// A rollback must not discard work another connection already committed. Only
/// the tables this transaction wrote are restored.
#[test]
fn rollback_leaves_other_tables_alone() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Other (id INT NOT NULL, PRIMARY KEY (id))",
    );

    let mut concurrent = Session::new(ServerConfig::default(), 2);
    concurrent.current_database = Some("testdb".to_owned());

    run(&engine, &mut session, "BEGIN");
    run(
        &engine,
        &mut session,
        "INSERT INTO Widget (id, name) VALUES ('z', 'zeta')",
    );

    // Another connection commits to a different table while the transaction
    // is open.
    run(&engine, &mut concurrent, "INSERT INTO Other VALUES (1)");

    run(&engine, &mut session, "ROLLBACK");

    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["3"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Other"),
        vec![vec!["1"]],
        "another connection's committed work must survive"
    );
}

/// Under autocommit a stray SAVEPOINT must not open a transaction, or a later
/// ROLLBACK would undo committed work.
#[test]
fn a_savepoint_outside_a_transaction_does_not_open_one() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(&engine, &mut session, "SAVEPOINT stray");
    run(
        &engine,
        &mut session,
        "INSERT INTO Widget (id, name) VALUES ('z', 'zeta')",
    );
    run(&engine, &mut session, "ROLLBACK");

    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Widget"),
        vec![vec!["4"]]
    );
}

// ---------------------------------------------------------------------------
// Multi-table DML, set operators and positional windows. Motte #0028.
// ---------------------------------------------------------------------------

/// The shape the migrations use 57 times: update one table from a join with
/// another.
#[test]
fn a_multi_table_update_writes_through_a_join() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Bump (id VARCHAR(21) NOT NULL, amount INT, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Bump VALUES ('a', 10), ('c', 20)",
    );

    run(
        &engine,
        &mut session,
        "UPDATE Widget JOIN Bump ON Bump.id = Widget.id SET Widget.count = Bump.amount",
    );

    let after = rows(
        &engine,
        &mut session,
        "SELECT id, count FROM Widget ORDER BY id",
    );
    assert_eq!(
        after,
        vec![vec!["a", "10"], vec!["b", "1"], vec!["c", "20"]]
    );
}

#[test]
fn a_multi_table_update_can_write_to_both_sides() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE A (id INT NOT NULL, v INT, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE B (id INT NOT NULL, v INT, PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO A VALUES (1, 0)");
    run(&engine, &mut session, "INSERT INTO B VALUES (1, 0)");

    run(
        &engine,
        &mut session,
        "UPDATE A JOIN B ON B.id = A.id SET A.v = 7, B.v = 9",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT v FROM A"),
        vec![vec!["7"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT v FROM B"),
        vec![vec!["9"]]
    );
}

/// A WHERE on a joined UPDATE selects which base rows are written.
#[test]
fn a_multi_table_update_honours_its_where_clause() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Bump (id VARCHAR(21) NOT NULL, amount INT, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Bump VALUES ('a', 10), ('c', 20)",
    );

    run(
        &engine,
        &mut session,
        "UPDATE Widget JOIN Bump ON Bump.id = Widget.id SET Widget.count = 0 WHERE Bump.amount > 15",
    );

    let after = rows(
        &engine,
        &mut session,
        "SELECT id, count FROM Widget ORDER BY id",
    );
    assert_eq!(after, vec![vec!["a", "3"], vec!["b", "1"], vec!["c", "0"]]);
}

#[test]
fn a_multi_table_delete_removes_from_the_named_table() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Doomed (id VARCHAR(21) NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Doomed VALUES ('a'), ('c')",
    );

    run(
        &engine,
        &mut session,
        "DELETE Widget FROM Widget JOIN Doomed ON Doomed.id = Widget.id",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget ORDER BY id"),
        vec![vec!["b"]]
    );
    // The joined-to table is untouched unless it is named.
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Doomed"),
        vec![vec!["2"]]
    );
}

#[test]
fn a_multi_table_delete_can_name_both_tables() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE A (id INT NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE B (id INT NOT NULL, PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO A VALUES (1), (2)");
    run(&engine, &mut session, "INSERT INTO B VALUES (1)");

    run(
        &engine,
        &mut session,
        "DELETE A, B FROM A JOIN B ON B.id = A.id",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM A"),
        vec![vec!["2"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM B"),
        vec![vec!["0"]]
    );
}

#[test]
fn intersect_and_except_compare_result_sets() {
    let (engine, mut session) = engine();
    sales(&engine, &mut session);

    let both = rows(
        &engine,
        &mut session,
        "SELECT rep FROM Sale WHERE region = 'north'
         INTERSECT
         SELECT rep FROM Sale WHERE region = 'south'",
    );
    assert_eq!(both, vec![vec!["ann"]]);

    let only_north = rows(
        &engine,
        &mut session,
        "SELECT rep FROM Sale WHERE region = 'north'
         EXCEPT
         SELECT rep FROM Sale WHERE region = 'south'",
    );
    assert_eq!(only_north, vec![vec!["bob"]]);
}

#[test]
fn lag_and_lead_read_neighbouring_rows() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE S (id INT NOT NULL, v INT, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES (1, 10), (2, 20), (3, 30)",
    );

    let shifted = rows(
        &engine,
        &mut session,
        "SELECT id, LAG(v) OVER (ORDER BY id), LEAD(v) OVER (ORDER BY id) FROM S ORDER BY id",
    );
    assert_eq!(
        shifted,
        vec![
            vec!["1", "NULL", "20"],
            vec!["2", "10", "30"],
            vec!["3", "20", "NULL"],
        ]
    );
}

#[test]
fn lag_takes_an_offset_and_a_default() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE S (id INT NOT NULL, v INT, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES (1, 10), (2, 20), (3, 30)",
    );

    let shifted = rows(
        &engine,
        &mut session,
        "SELECT LAG(v, 2, 0) OVER (ORDER BY id) FROM S ORDER BY id",
    );
    assert_eq!(shifted, vec![vec!["0"], vec!["0"], vec!["10"]]);
}

/// MySQL's default frame ends at the current row, so LAST_VALUE is the current
/// row rather than the end of the partition. Reading it as the partition's last
/// value is a classic trap.
#[test]
fn first_value_and_last_value_follow_the_default_frame() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE S (id INT NOT NULL, v INT, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES (1, 10), (2, 20), (3, 30)",
    );

    let framed = rows(
        &engine,
        &mut session,
        "SELECT FIRST_VALUE(v) OVER (ORDER BY id), LAST_VALUE(v) OVER (ORDER BY id)
         FROM S ORDER BY id",
    );
    assert_eq!(
        framed,
        vec![vec!["10", "10"], vec!["10", "20"], vec!["10", "30"]]
    );
}

// --- Multi-table DML, set operators and positional windows -----------------
//
// Every test below is a regression for a review finding where the engine
// produced a *wrong answer* rather than an error — the one failure mode a
// test database cannot have.

/// Two tables joined by a foreign key, for the multi-table DML tests.
fn parent_child(engine: &Engine, session: &mut Session) {
    run(
        engine,
        session,
        "CREATE TABLE Parent (
            id INTEGER NOT NULL,
            label VARCHAR(20) NOT NULL,
            PRIMARY KEY (id)
        )",
    );
    run(
        engine,
        session,
        "CREATE TABLE Child (
            id INTEGER NOT NULL,
            parentId INTEGER NOT NULL,
            note VARCHAR(20) NULL,
            PRIMARY KEY (id),
            CONSTRAINT Child_parent FOREIGN KEY (parentId)
                REFERENCES Parent(id) ON DELETE CASCADE ON UPDATE CASCADE
        )",
    );
    run(
        engine,
        session,
        "INSERT INTO Parent (id, label) VALUES (1, 'one'), (2, 'two')",
    );
    run(
        engine,
        session,
        "INSERT INTO Child (id, parentId, note) VALUES (10, 1, 'a'), (11, 1, 'b'), (12, 2, 'c')",
    );
}

#[test]
fn a_multi_table_delete_without_a_target_is_refused() {
    // `DELETE FROM a JOIN b` used to empty *both* tables. MySQL rejects the
    // syntax, so an error is the only safe answer.
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    let sql = "DELETE FROM Parent JOIN Child ON Child.parentId = Parent.id";
    let err = engine.execute(&mut session, sql, &[]).unwrap_err();
    assert_eq!(err.code, 1064);

    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Parent").len(),
        2
    );
    assert_eq!(rows(&engine, &mut session, "SELECT id FROM Child").len(), 3);
}

#[test]
fn a_multi_table_delete_removes_only_the_named_table() {
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    let removed = affected(
        &engine,
        &mut session,
        "DELETE Child FROM Child JOIN Parent ON Child.parentId = Parent.id \
         WHERE Parent.label = 'one'",
    );
    assert_eq!(removed, 2);
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Parent").len(),
        2
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Child ORDER BY id"),
        vec![vec!["12"]]
    );
}

#[test]
fn a_multi_table_delete_resolves_aliases_to_their_base_table() {
    // The target is named by its alias; the rows live in the base table. Using
    // the alias as a catalog key silently found nothing.
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    let removed = affected(
        &engine,
        &mut session,
        "DELETE c FROM Child c JOIN Parent p ON c.parentId = p.id WHERE p.id = 2",
    );
    assert_eq!(removed, 1);
    assert_eq!(rows(&engine, &mut session, "SELECT id FROM Child").len(), 2);
}

#[test]
fn a_multi_table_delete_cascades_after_every_named_table_is_cleared() {
    // Cascading between removals shifted the row indices the remaining
    // deletions were captured against, destroying the wrong rows.
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    // Both tables are named, so a cascade run between the two removals would
    // shift the indices the second one was captured against.
    let removed = affected(
        &engine,
        &mut session,
        "DELETE p, c FROM Parent p JOIN Child c ON c.parentId = p.id \
         WHERE p.id = 2 AND c.id = 12",
    );
    assert_eq!(removed, 2);
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Parent ORDER BY id"),
        vec![vec!["1"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Child ORDER BY id"),
        vec![vec!["10"], vec!["11"]]
    );
}

#[test]
fn a_multi_table_delete_of_one_table_through_two_aliases() {
    // Two aliases are two join sources over one row vector. Deleting per
    // source applied the second set's indices to an already-shortened table,
    // which destroyed rows the statement never selected.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE N (id INTEGER NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO N (id) VALUES (1), (2), (3), (4), (5), (6)",
    );

    let removed = affected(
        &engine,
        &mut session,
        "DELETE x, y FROM N x JOIN N y ON x.id = 1 AND y.id = 3",
    );
    assert_eq!(removed, 2);
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM N ORDER BY id"),
        vec![vec!["2"], vec!["4"], vec!["5"], vec!["6"]]
    );
}

#[test]
fn naming_one_table_twice_in_a_multi_table_delete_is_refused() {
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    let err = engine
        .execute(
            &mut session,
            "DELETE Child, Child FROM Child JOIN Parent ON Child.parentId = Parent.id",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1066);
    assert_eq!(rows(&engine, &mut session, "SELECT id FROM Child").len(), 3);
}

#[test]
fn a_join_whose_sources_share_a_name_is_refused() {
    // `source_index` takes the first match, so without this the write lands on
    // whichever table happens to come first — not the one that was named.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INTEGER NOT NULL, v INTEGER NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE T2 (id INTEGER NOT NULL, v INTEGER NOT NULL, PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO T VALUES (1, 1), (2, 2)");
    run(
        &engine,
        &mut session,
        "INSERT INTO T2 VALUES (1, 1), (2, 2)",
    );

    let err = engine
        .execute(
            &mut session,
            "DELETE t FROM T2 t JOIN T ON 1 = 1 WHERE t.id = 1",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1066);

    let err = engine
        .execute(&mut session, "UPDATE T2 t JOIN T ON 1 = 1 SET t.v = 9", &[])
        .unwrap_err();
    assert_eq!(err.code, 1066);

    // Neither table was touched.
    assert_eq!(
        rows(&engine, &mut session, "SELECT v FROM T2 ORDER BY id"),
        vec![vec!["1"], vec!["2"]]
    );
}

#[test]
fn the_canonical_delete_star_spelling_works() {
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    let removed = affected(
        &engine,
        &mut session,
        "DELETE c.* FROM Child c JOIN Parent p ON c.parentId = p.id WHERE p.id = 2",
    );
    assert_eq!(removed, 1);
    assert_eq!(rows(&engine, &mut session, "SELECT id FROM Child").len(), 2);
}

#[test]
fn a_joined_update_applies_every_assignment_to_the_same_row() {
    // Each assignment used to be applied against a separately re-read row, so
    // only the last one survived.
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    let updated = affected(
        &engine,
        &mut session,
        "UPDATE Child c JOIN Parent p ON c.parentId = p.id \
         SET c.note = 'x', c.parentId = 2 WHERE c.id = 10",
    );
    assert_eq!(updated, 1);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT note, parentId FROM Child WHERE id = 10"
        ),
        vec![vec!["x", "2"]]
    );
}

#[test]
fn a_joined_update_counts_rows_not_assignments() {
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    let updated = affected(
        &engine,
        &mut session,
        "UPDATE Child c JOIN Parent p ON c.parentId = p.id \
         SET c.note = 'y', c.id = c.id WHERE p.id = 1",
    );
    assert_eq!(updated, 2, "two rows changed, not four assignments");
}

#[test]
fn a_joined_update_enforces_foreign_keys() {
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    let err = engine
        .execute(
            &mut session,
            "UPDATE Child c JOIN Parent p ON c.parentId = p.id SET c.parentId = 99",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1452);
    // The refusal left every row as it was.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT parentId FROM Child ORDER BY id"
        ),
        vec![vec!["1"], vec!["1"], vec!["2"]]
    );
}

#[test]
fn a_joined_update_cascades_to_children() {
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    let updated = affected(
        &engine,
        &mut session,
        "UPDATE Parent p JOIN Child c ON c.parentId = p.id SET p.id = 7 WHERE p.id = 2",
    );
    assert_eq!(updated, 1);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT parentId FROM Child WHERE id = 12"
        ),
        vec![vec!["7"]]
    );
}

#[test]
fn an_ambiguous_unqualified_update_target_is_refused() {
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);
    run(
        engine.as_ref(),
        &mut session,
        "ALTER TABLE Parent ADD COLUMN note VARCHAR(20) NULL",
    );

    let err = engine
        .execute(
            &mut session,
            "UPDATE Child c JOIN Parent p ON c.parentId = p.id SET note = 'z'",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1052);
    assert_eq!(
        rows(&engine, &mut session, "SELECT note FROM Child ORDER BY id"),
        vec![vec!["a"], vec!["b"], vec!["c"]]
    );
}

#[test]
fn a_joined_update_refuses_to_write_a_generated_column() {
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);
    run(
        engine.as_ref(),
        &mut session,
        "ALTER TABLE Child ADD COLUMN shout VARCHAR(40) \
         GENERATED ALWAYS AS (UPPER(note)) STORED",
    );

    for sql in [
        "UPDATE Child c JOIN Parent p ON c.parentId = p.id SET c.shout = 'NO'",
        // Second position: the check used to run only for the first
        // assignment to a table, so this one wrote nothing and said nothing.
        "UPDATE Child c JOIN Parent p ON c.parentId = p.id SET c.note = 'q', c.shout = 'NO'",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 3105, "expected 3105 for: {sql}");
    }

    // Nothing was written on the way to either refusal.
    assert_eq!(
        rows(&engine, &mut session, "SELECT note FROM Child ORDER BY id"),
        vec![vec!["a"], vec!["b"], vec!["c"]]
    );
}

#[test]
fn intersect_all_and_except_all_are_multiset_operations() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Tally (id INTEGER NOT NULL, name VARCHAR(10) NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Tally (id, name) VALUES (1, 'dup'), (2, 'dup'), (3, 'dup'), (4, 'solo')",
    );

    // Three 'dup' rows on the left, two on the right: ALL keeps two, not three.
    let kept = rows(
        &engine,
        &mut session,
        "SELECT name FROM Tally WHERE name = 'dup' \
         INTERSECT ALL \
         SELECT name FROM Tally WHERE id IN (1, 2)",
    );
    assert_eq!(kept, vec![vec!["dup"], vec!["dup"]]);

    // EXCEPT ALL cancels one occurrence per matching right-hand row.
    let left = rows(
        &engine,
        &mut session,
        "SELECT name FROM Tally WHERE name = 'dup' \
         EXCEPT ALL \
         SELECT name FROM Tally WHERE id = 1",
    );
    assert_eq!(left, vec![vec!["dup"], vec!["dup"]]);

    // Without ALL both stay set operations.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT name FROM Tally WHERE name = 'dup' \
             INTERSECT \
             SELECT name FROM Tally WHERE id IN (1, 2)"
        )
        .len(),
        1
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT name FROM Tally WHERE name = 'dup' \
             EXCEPT \
             SELECT name FROM Tally WHERE id = 1"
        )
        .len(),
        0
    );
}

#[test]
fn a_non_constant_lag_offset_is_refused() {
    // Defaulting a computed offset to 1 answers a different question than the
    // one asked, without saying so.
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let err = engine
        .execute(
            &mut session,
            "SELECT LAG(name, count) OVER (ORDER BY id) FROM Widget",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1235);

    let err = engine
        .execute(
            &mut session,
            "SELECT LEAD(name, 1, name) OVER (ORDER BY id) FROM Widget",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1235);

    // A constant offset still works.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT LAG(name, 1, 'none') OVER (ORDER BY id) FROM Widget"
        ),
        vec![vec!["none"], vec!["alpha"], vec!["beta"]]
    );
}

#[test]
fn dml_modifiers_mysql_refuses_are_refused_here_too() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Tag (wid VARCHAR(21), label VARCHAR(20))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Tag VALUES ('a', 'keep')",
    );

    // "The first row" is not defined across a join, so MySQL allows neither
    // modifier on the multi-table forms. Accepting them would have to invent
    // an order the user never asked for.
    for sql in [
        "UPDATE Widget JOIN Tag ON Tag.wid = Widget.id SET count = 1 LIMIT 1",
        "UPDATE Widget JOIN Tag ON Tag.wid = Widget.id SET count = 1 ORDER BY Widget.id",
        "DELETE Widget FROM Widget JOIN Tag ON Tag.wid = Widget.id LIMIT 1",
        "DELETE FROM Widget USING Widget JOIN Tag ON Tag.wid = Widget.id ORDER BY Widget.id",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1235, "expected ER_NOT_SUPPORTED_YET for: {sql}");
    }

    // A LIMIT that is not an exact row count is refused rather than coerced
    // into one. `1.9` truncating to a single row and `'2'` reading as two are
    // the wrong number of rows deleted, quietly.
    for sql in [
        "DELETE FROM Widget LIMIT -1",
        "DELETE FROM Widget LIMIT 'x'",
        "DELETE FROM Widget LIMIT 1.9",
        "DELETE FROM Widget LIMIT '2'",
        "DELETE FROM Widget LIMIT NULL",
        "UPDATE Widget SET count = 1 LIMIT -1",
        "UPDATE Widget SET count = 1 LIMIT 0.5",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1064, "expected a syntax error for: {sql}");
    }

    // An ORDER BY that cannot be resolved is an error, not an ordering of
    // nothing — silently falling back to storage order would let the LIMIT
    // pick an arbitrary row and call it the first.
    for sql in [
        "DELETE FROM Widget ORDER BY nope LIMIT 1",
        "UPDATE Widget SET count = 1 ORDER BY nope LIMIT 1",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1054, "{sql}");
    }

    // RETURNING is still not implemented, and still says so.
    let err = engine
        .execute(&mut session, "DELETE FROM Widget RETURNING id", &[])
        .unwrap_err();
    assert_eq!(err.code, 1235);

    // Every row survived every refusal.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT count FROM Widget ORDER BY id"
        ),
        vec![vec!["3"], vec!["1"], vec!["2"]]
    );
}

#[test]
fn an_out_of_scope_qualifier_is_an_unknown_column() {
    // The evaluator used to fall back to an unqualified lookup, so a wrong
    // alias bound to another table's same-named column and drove real writes.
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    for sql in [
        "SELECT zz.id FROM Child",
        "UPDATE Child c JOIN Parent p ON c.parentId = p.id SET c.note = 'x' WHERE zz.id = 10",
        "DELETE c FROM Child c JOIN Parent p ON c.parentId = p.id WHERE zz.id = 10",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1054, "expected ER_BAD_FIELD_ERROR for: {sql}");
    }

    assert_eq!(
        rows(&engine, &mut session, "SELECT note FROM Child ORDER BY id"),
        vec![vec!["a"], vec!["b"], vec!["c"]]
    );
}

#[test]
fn a_self_join_update_counts_each_physical_row_once() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE S (id INTEGER NOT NULL, v INTEGER NOT NULL, PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO S VALUES (1, 1), (2, 2)");

    let updated = affected(
        &engine,
        &mut session,
        "UPDATE S x JOIN S y ON y.id = x.id SET x.v = 7",
    );
    assert_eq!(updated, 2, "two rows, not one per alias");
    assert_eq!(
        rows(&engine, &mut session, "SELECT v FROM S ORDER BY id"),
        vec![vec!["7"], vec!["7"]]
    );
}

#[test]
fn a_windowed_aggregate_with_order_by_runs_to_the_current_row() {
    // The default frame is RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW.
    // Folding the whole partition instead reports the final total on every
    // row, which is a different query than the one asked.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE W (id INTEGER NOT NULL, g INTEGER NOT NULL, v INTEGER NOT NULL,
         PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO W VALUES (1, 1, 10), (2, 1, 20), (3, 2, 30)",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT SUM(v) OVER (ORDER BY id) FROM W ORDER BY id"
        ),
        vec![vec!["10"], vec!["30"], vec!["60"]]
    );

    // In RANGE mode the current row means the last of its peer group, so tied
    // keys share one running total.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) OVER (ORDER BY g) FROM W ORDER BY id"
        ),
        vec![vec!["2"], vec!["2"], vec!["3"]]
    );

    // LAST_VALUE reads the end of that same frame.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT LAST_VALUE(v) OVER (ORDER BY g) FROM W ORDER BY id"
        ),
        vec![vec!["20"], vec!["20"], vec!["30"]]
    );

    // Without ORDER BY the frame is the whole partition.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT SUM(v) OVER (PARTITION BY g) FROM W ORDER BY id"
        ),
        vec![vec!["30"], vec!["30"], vec!["30"]]
    );
}

#[test]
fn a_negative_lag_offset_is_refused() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let err = engine
        .execute(
            &mut session,
            "SELECT LAG(name, -1) OVER (ORDER BY id) FROM Widget",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1210);
}

// --- Running the real migrations -------------------------------------------
//
// Everything below was found by pointing `prisma migrate deploy` at the engine
// and fixing whatever it hit next. Each is a feature the real 604-migration
// history needs, or a bug that history exposed.

#[test]
fn a_subquery_in_values_does_not_deadlock() {
    // The writer held the catalog's write lock while evaluating, and the
    // subquery needed a read lock to answer. `RwLock` is not reentrant, so the
    // statement hung forever rather than failing.
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Pick (id VARCHAR(21) NOT NULL, chosen VARCHAR(120) NULL, PRIMARY KEY (id))",
    );

    run(
        &engine,
        &mut session,
        "INSERT INTO Pick (id, chosen) VALUES ('p1', (SELECT name FROM Widget WHERE id = 'a'))",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT chosen FROM Pick"),
        vec![vec!["alpha"]]
    );

    // The same hazard on the UPDATE and DELETE paths.
    run(
        &engine,
        &mut session,
        "UPDATE Pick SET chosen = (SELECT name FROM Widget WHERE id = 'b')",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT chosen FROM Pick"),
        vec![vec!["beta"]]
    );
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "DELETE FROM Pick WHERE chosen = (SELECT name FROM Widget WHERE id = 'b')"
        ),
        1
    );
}

#[test]
fn insert_select_copies_rows() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Copy (id VARCHAR(21) NOT NULL, name VARCHAR(120) NOT NULL, PRIMARY KEY (id))",
    );

    let inserted = affected(
        &engine,
        &mut session,
        "INSERT INTO Copy (id, name) SELECT id, name FROM Widget WHERE count > 1",
    );
    assert_eq!(inserted, 2);
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Copy ORDER BY id"),
        vec![vec!["a"], vec!["c"]]
    );

    // IGNORE still skips a duplicate rather than failing the statement.
    let again = affected(
        &engine,
        &mut session,
        "INSERT IGNORE INTO Copy (id, name) SELECT id, name FROM Widget",
    );
    assert_eq!(again, 1);
}

#[test]
fn a_multi_statement_command_runs_every_statement() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let executed = engine.execute_many(
        &mut session,
        "UPDATE Widget SET count = 10 WHERE id = 'a'; \
         SELECT count FROM Widget WHERE id = 'a'; \
         DELETE FROM Widget WHERE id = 'b';",
        &[],
    );
    assert!(executed.error.is_none());
    assert_eq!(executed.outcomes.len(), 3);
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget ORDER BY id"),
        vec![vec!["a"], vec!["c"]]
    );
}

#[test]
fn a_semicolon_inside_a_literal_is_not_a_separator() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let executed = engine.execute_many(
        &mut session,
        "UPDATE Widget SET note = 'a;b' WHERE id = 'a'; -- and a; comment\n\
         SELECT note FROM Widget WHERE id = 'a'",
        &[],
    );
    assert!(executed.error.is_none());
    assert_eq!(executed.outcomes.len(), 2);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT note FROM Widget WHERE id = 'a'"
        ),
        vec![vec!["a;b"]]
    );
}

#[test]
fn a_multi_statement_command_stops_at_the_first_error() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let executed = engine.execute_many(
        &mut session,
        "UPDATE Widget SET count = 5 WHERE id = 'a'; \
         SELECT * FROM Nope; \
         DELETE FROM Widget",
        &[],
    );
    assert!(executed.error.is_some());

    // The statement before the error stands; the one after it never ran.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT count FROM Widget WHERE id = 'a'"
        ),
        vec![vec!["5"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget").len(),
        3
    );
}

#[test]
fn a_cte_can_be_selected_from_and_joined_to() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "WITH big AS (SELECT id, count FROM Widget WHERE count > 1) \
             SELECT id FROM big ORDER BY id"
        ),
        vec![vec!["a"], vec!["c"]]
    );

    // Named columns, and a join back to the base table.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "WITH totals (wid, n) AS (SELECT id, count FROM Widget) \
             SELECT w.name, t.n FROM Widget w JOIN totals t ON t.wid = w.id \
             WHERE t.n = 3"
        ),
        vec![vec!["alpha", "3"]]
    );
}

#[test]
fn a_cte_can_feed_an_update() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let updated = affected(
        &engine,
        &mut session,
        "WITH ranked AS (SELECT id, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM Widget) \
         UPDATE Widget w JOIN ranked r ON r.id = w.id SET w.count = r.rn",
    );
    assert_eq!(updated, 3);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT count FROM Widget ORDER BY id"
        ),
        vec![vec!["1"], vec!["2"], vec!["3"]]
    );
}

#[test]
fn a_recursive_cte_is_refused() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    let err = engine
        .execute(
            &mut session,
            "WITH RECURSIVE n AS (SELECT 1 AS x) SELECT x FROM n",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1235);
}

#[test]
fn named_locks_are_held_per_connection() {
    // Prisma's migration engine takes GET_LOCK before it applies anything.
    let (engine, mut first) = engine();
    let mut second = Session::new(ServerConfig::default(), 2);
    second.current_database = Some("testdb".to_owned());

    assert_eq!(
        rows(&engine, &mut first, "SELECT GET_LOCK('m', 1)"),
        vec![vec!["1"]]
    );
    // Re-taking a lock this connection already holds succeeds; another
    // connection is told no rather than waiting for a holder in the same
    // process that cannot release while it waits.
    assert_eq!(
        rows(&engine, &mut first, "SELECT GET_LOCK('m', 1)"),
        vec![vec!["1"]]
    );
    assert_eq!(
        rows(&engine, &mut second, "SELECT GET_LOCK('m', 1)"),
        vec![vec!["0"]]
    );
    assert_eq!(
        rows(&engine, &mut second, "SELECT IS_USED_LOCK('m')"),
        vec![vec!["1"]]
    );

    // Releasing someone else's lock reports 0 and does not release it.
    assert_eq!(
        rows(&engine, &mut second, "SELECT RELEASE_LOCK('m')"),
        vec![vec!["0"]]
    );
    assert_eq!(
        rows(&engine, &mut first, "SELECT RELEASE_LOCK('m')"),
        vec![vec!["1"]]
    );
    assert_eq!(
        rows(&engine, &mut second, "SELECT IS_FREE_LOCK('m')"),
        vec![vec!["1"]]
    );
    // An unheld lock releases to NULL, not 0.
    assert_eq!(
        rows(&engine, &mut first, "SELECT RELEASE_LOCK('m')"),
        vec![vec!["NULL"]]
    );
}

#[test]
fn generated_values_are_unique_and_deterministic() {
    let (first_engine, mut second_session) = engine();
    let (engine, session) = (&first_engine, &mut second_session);

    let first = rows(engine, session, "SELECT UUID(), UUID(), MD5('abc')");
    // Two calls in one statement must not collide, and neither must their
    // prefixes: the migrations derive ids with SUBSTRING(REPLACE(UUID(), ...)).
    assert_ne!(first[0][0], first[0][1]);
    assert_ne!(first[0][0][..8], first[0][1][..8]);
    assert_eq!(first[0][2], "900150983cd24fb0d6963f7d28e17f72");

    // A fresh engine repeats the sequence, so a test can assert on an id.
    let (other, mut other_session) = fresh_engine();
    assert_eq!(
        rows(&other, &mut other_session, "SELECT UUID()")[0][0],
        first[0][0]
    );

    // RAND stays inside [0, 1) and moves.
    let r = rows(engine, session, "SELECT RAND(), RAND()");
    let (a, b): (f64, f64) = (r[0][0].parse().unwrap(), r[0][1].parse().unwrap());
    assert!((0.0..1.0).contains(&a) && (0.0..1.0).contains(&b));
    assert_ne!(a, b);
}

#[test]
fn regular_expressions_match_and_replace() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT REGEXP_REPLACE('Hello World', '[^a-zA-Z]+', '-')"
        ),
        vec![vec!["Hello-World"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM Widget WHERE name REGEXP '^(al|ga)' ORDER BY id"
        ),
        vec![vec!["a"], vec!["c"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM Widget WHERE name NOT REGEXP '^(al|ga)'"
        ),
        vec![vec!["b"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT REGEXP_SUBSTR('a1b22c', '[0-9]+')"
        ),
        vec![vec!["1"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT REGEXP_INSTR('a1b22c', '[0-9]{2}')"
        ),
        vec![vec!["4"]]
    );

    // A pattern the engine cannot compile is an error, never a silent no-match
    // that would rewrite or skip the wrong rows.
    let err = engine
        .execute(&mut session, "SELECT REGEXP_LIKE('a', '[')", &[])
        .unwrap_err();
    assert_eq!(err.code, 3692);
}

#[test]
fn a_values_constructor_is_a_result_in_its_own_right() {
    let (engine, mut session) = engine();
    assert_eq!(
        rows(&engine, &mut session, "VALUES ROW('a'), ROW('b')"),
        vec![vec!["a"], vec!["b"]]
    );
}

#[test]
fn on_update_current_timestamp_restamps_a_changed_row() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Stamped (
            id INTEGER NOT NULL,
            v INTEGER NOT NULL,
            updatedAt DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                ON UPDATE CURRENT_TIMESTAMP(3),
            PRIMARY KEY (id)
        )",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Stamped (id, v, updatedAt) VALUES (1, 1, '2000-01-01 00:00:00.000')",
    );

    // A statement that changes nothing leaves the stamp alone, as MySQL does.
    run(&engine, &mut session, "UPDATE Stamped SET v = 1");
    assert_eq!(
        rows(&engine, &mut session, "SELECT updatedAt FROM Stamped")[0][0],
        "2000-01-01 00:00:00.000"
    );

    run(&engine, &mut session, "UPDATE Stamped SET v = 2");
    assert_ne!(
        rows(&engine, &mut session, "SELECT updatedAt FROM Stamped")[0][0],
        "2000-01-01 00:00:00.000"
    );

    // An explicit value wins over the restamp.
    run(
        &engine,
        &mut session,
        "UPDATE Stamped SET v = 3, updatedAt = '2001-01-01 00:00:00.000'",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT updatedAt FROM Stamped")[0][0],
        "2001-01-01 00:00:00.000"
    );
}

#[test]
fn an_alias_qualifies_a_single_table_update_or_delete() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert_eq!(
        affected(
            &engine,
            &mut session,
            "UPDATE Widget w SET w.count = 9 WHERE w.id = 'a'"
        ),
        1
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT count FROM Widget WHERE id = 'a'"
        ),
        vec![vec!["9"]]
    );
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "DELETE FROM Widget w WHERE w.id = 'b'"
        ),
        1
    );
}

#[test]
fn a_foreign_key_creates_the_index_mysql_would() {
    // MySQL indexes every foreign key, and Prisma renames that index in a
    // later migration — which fails outright if it was never created.
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "ALTER TABLE Child RENAME INDEX Child_parent TO Child_parent_index",
    );
    run(
        &engine,
        &mut session,
        "DROP INDEX Child_parent_index ON Child",
    );

    let err = engine
        .execute(&mut session, "DROP INDEX Child_parent_index ON Child", &[])
        .unwrap_err();
    assert_eq!(err.code, 1091);
}

#[test]
fn an_inline_unique_index_keeps_the_name_it_was_given() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Named (
            id INTEGER NOT NULL,
            a VARCHAR(20) NOT NULL,
            b VARCHAR(20) NOT NULL,
            UNIQUE INDEX Named_a_b_key (a, b),
            PRIMARY KEY (id)
        )",
    );
    // Stored under an invented name, this could not be found again.
    run(&engine, &mut session, "DROP INDEX Named_a_b_key ON Named");
}

#[test]
fn insert_ignore_fills_a_not_null_column_that_has_no_default() {
    // Strict mode makes the plain statement an error; IGNORE downgrades it to
    // a warning and writes the type's implicit default, which is what the real
    // seed migrations depend on.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Ordered (id INTEGER NOT NULL, orderNo INTEGER NOT NULL, PRIMARY KEY (id))",
    );

    engine
        .execute(&mut session, "INSERT INTO Ordered (id) VALUES (1)", &[])
        .unwrap_err();
    run(
        &engine,
        &mut session,
        "INSERT IGNORE INTO Ordered (id) VALUES (1)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT orderNo FROM Ordered"),
        vec![vec!["0"]]
    );
}

#[test]
fn only_full_group_by_allows_a_column_the_grouping_determines() {
    // MySQL permits a column that a join equality fixes for the whole group.
    let (engine, mut session) = engine();
    parent_child(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT p.id, COUNT(*) FROM Child c \
             LEFT JOIN Parent p ON p.id = c.parentId \
             GROUP BY parentId ORDER BY p.id"
        ),
        vec![vec!["1", "2"], vec!["2", "1"]]
    );

    // A column nothing determines is still an error.
    let err = engine
        .execute(
            &mut session,
            "SELECT c.note, COUNT(*) FROM Child c \
             LEFT JOIN Parent p ON p.id = c.parentId GROUP BY parentId",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1055);
}

#[test]
fn cast_accepts_the_mysql_integer_spellings() {
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST('42' AS SIGNED), CAST('7' AS UNSIGNED)"
        ),
        vec![vec!["42", "7"]]
    );
}

#[test]
fn substring_trim_and_position_work_in_keyword_form() {
    // sqlparser models these as their own nodes, so the keyword spellings used
    // to miss the function table entirely.
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT SUBSTRING('abcdef' FROM 2 FOR 3), \
                    TRIM(BOTH ' ' FROM '  hi  '), \
                    TRIM(LEADING ' ' FROM '  hi'), \
                    POSITION('c' IN 'abc')"
        ),
        vec![vec!["bcd", "hi", "hi", "3"]]
    );
}

#[test]
fn only_full_group_by_still_rejects_the_preserved_side_of_an_outer_join() {
    // A LEFT JOIN's ON does not hold on the NULL-extended rows, so the
    // preserved side is not determined by the null-supplying one: every
    // unmatched row lands in the same group and the engine would pick one of
    // them arbitrarily.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE L (id INTEGER NOT NULL, name VARCHAR(10) NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE R (id INTEGER NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO L VALUES (1,'one'),(2,'two'),(3,'three')",
    );
    run(&engine, &mut session, "INSERT INTO R VALUES (1)");

    let err = engine
        .execute(
            &mut session,
            "SELECT R.id, L.id, COUNT(*) FROM L LEFT JOIN R ON L.id = R.id GROUP BY R.id",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1055);

    // The other direction is fine: grouping by the preserved side determines
    // the null-supplying one, because there it is NULL — a single value.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT L.id, R.id FROM L LEFT JOIN R ON L.id = R.id GROUP BY L.id ORDER BY L.id"
        ),
        vec![vec!["1", "1"], vec!["2", "NULL"], vec!["3", "NULL"]]
    );
}

#[test]
fn rand_spreads_across_the_range() {
    // A counter run through a weak mixer produced nearly equal values, so
    // `ORDER BY RAND()` returned scan order and `WHERE RAND() < 0.5` matched
    // every row or none.
    let (engine, mut session) = engine();
    let values: Vec<f64> = rows(
        &engine,
        &mut session,
        "SELECT RAND(), RAND(), RAND(), RAND(), RAND(), RAND(), RAND(), RAND()",
    )[0]
    .iter()
    .map(|v| v.parse().unwrap())
    .collect();

    assert!(values.iter().all(|v| (0.0..1.0).contains(v)));
    let below = values.iter().filter(|v| **v < 0.5).count();
    assert!(
        (1..values.len()).contains(&below),
        "all on one side of 0.5: {values:?}"
    );

    // A seed restarts the sequence rather than repeating one value.
    let seeded = rows(&engine, &mut session, "SELECT RAND(7), RAND()");
    assert_ne!(seeded[0][0], seeded[0][1]);
    let again = rows(&engine, &mut session, "SELECT RAND(7)");
    assert_eq!(again[0][0], seeded[0][0]);
}

#[test]
fn the_optional_regexp_arguments_change_the_answer() {
    // Accepting and ignoring these answered a different question: 'c' asks for
    // a case-sensitive match, and an occurrence of 2 rewrites only the second.
    let (engine, mut session) = engine();

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT REGEXP_INSTR('abcabc', 'a', 4), \
                    REGEXP_REPLACE('aaa', 'a', 'b', 1, 2), \
                    REGEXP_LIKE('ABC', 'abc', 'c'), \
                    REGEXP_LIKE('ABC', 'abc', 'i'), \
                    REGEXP_SUBSTR('a1b22c', '[0-9]+', 1, 2), \
                    REGEXP_REPLACE('a-b', '[^a-z]', '_', 1, 0, 'c')"
        ),
        vec![vec!["4", "aba", "0", "1", "22", "a_b"]]
    );

    // A match type that would change semantics this does not implement, and a
    // start position MySQL rejects, are errors rather than quiet answers.
    for (sql, code) in [
        ("SELECT REGEXP_LIKE('a', 'a', 'm')", 1235),
        ("SELECT REGEXP_SUBSTR('a', 'a', 0)", 1582),
        ("SELECT 'ABC' REGEXP BINARY 'abc'", 1235),
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, code, "for: {sql}");
    }
}

#[test]
fn a_cte_inside_a_subquery_is_in_scope() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM Widget WHERE id IN (WITH d AS (SELECT 'a' AS k) SELECT k FROM d)"
        ),
        vec![vec!["a"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT (WITH d AS (SELECT 7 AS n) SELECT n FROM d)"
        ),
        vec![vec!["7"]]
    );
}

#[test]
fn a_cte_is_evaluated_once_per_statement() {
    // Materialising it at two levels ran it twice, which is visible through
    // any function that advances state.
    let (engine, mut session) = engine();
    let first = rows(
        &engine,
        &mut session,
        "WITH c AS (SELECT UUID() AS u) SELECT u FROM c",
    );
    let (other, mut other_session) = fresh_engine();
    let bare = rows(&other, &mut other_session, "SELECT UUID()");
    assert_eq!(first[0][0], bare[0][0]);
}

#[test]
fn a_multi_statement_command_reports_what_already_ran() {
    // The statements before a failure have been applied and there is no
    // rollback, so reporting only the error would claim nothing happened.
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let executed = engine.execute_many(
        &mut session,
        "UPDATE Widget SET count = 7 WHERE id = 'a'; SELECT * FROM Nope; DELETE FROM Widget",
        &[],
    );
    assert_eq!(executed.outcomes.len(), 1);
    assert_eq!(executed.error.map(|e| e.code), Some(1146));
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget").len(),
        3
    );
}

#[test]
fn the_implicit_default_is_the_same_however_the_column_is_filled() {
    // Two implementations disagreed, so a column back-filled by ALTER TABLE
    // did not compare equal to one filled by INSERT IGNORE.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Defaults (id INTEGER NOT NULL, d DATE NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT IGNORE INTO Defaults (id) VALUES (1)",
    );
    run(
        &engine,
        &mut session,
        "ALTER TABLE Defaults ADD COLUMN d2 DATE NOT NULL",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM Defaults WHERE d = d2"
        ),
        vec![vec!["1"]]
    );
}

#[test]
fn cast_to_an_integer_reads_the_leading_number() {
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST('12abc' AS SIGNED), CAST('abc' AS SIGNED), CAST('-5x' AS SIGNED)"
        ),
        vec![vec!["12", "0", "-5"]]
    );

    // An unsigned cast of a negative value cannot be represented, so it is
    // refused rather than reported as a negative number.
    let err = engine
        .execute(&mut session, "SELECT CAST(-1 AS UNSIGNED)", &[])
        .unwrap_err();
    assert_eq!(err.code, 1235);
}

#[test]
fn unnamed_foreign_keys_get_distinct_names() {
    // One name for all of them meant the implicit indexes collided, and
    // dropping one would have dropped every one.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE P2 (id INTEGER NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE C2 (
            id INTEGER NOT NULL,
            pid INTEGER NOT NULL,
            qid INTEGER NOT NULL,
            PRIMARY KEY (id),
            FOREIGN KEY (pid) REFERENCES P2(id),
            FOREIGN KEY (qid) REFERENCES P2(id)
        )",
    );

    let names = rows(
        &engine,
        &mut session,
        "SELECT DISTINCT INDEX_NAME FROM information_schema.statistics \
         WHERE TABLE_NAME = 'C2' ORDER BY INDEX_NAME",
    );
    assert_eq!(
        names,
        vec![vec!["C2_ibfk_1"], vec!["C2_ibfk_2"], vec!["PRIMARY"]]
    );
}

#[test]
fn a_statement_describes_its_columns_before_it_runs() {
    // The prepare response carries this, and a client caches it: without it a
    // result has no column names and every lookup by name quietly finds none.
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let described = engine
        .describe(&session, "SELECT id, name FROM Widget", 0)
        .unwrap();
    assert_eq!(
        described.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        vec!["id", "name"]
    );

    // An alias is the name the client sees, and a parameter does not change
    // the shape — it is bound to NULL for the probe.
    let described = engine
        .describe(&session, "SELECT count AS n FROM Widget WHERE id = ?", 1)
        .unwrap();
    assert_eq!(
        described.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        vec!["n"]
    );
}

#[test]
fn describing_never_runs_a_write() {
    // The probe executes the statement, so anything that could write has to be
    // refused: describing it would apply it, and then executing would apply it
    // twice.
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    for sql in [
        "UPDATE Widget SET count = 99",
        "DELETE FROM Widget",
        "INSERT INTO Widget (id, name) VALUES ('z', 'zed')",
        "WITH c AS (SELECT id FROM Widget) UPDATE Widget w JOIN c ON c.id = w.id SET w.count = 42",
        "DROP TABLE Widget",
    ] {
        assert!(
            engine.describe(&session, sql, 0).is_none(),
            "described a write: {sql}"
        );
    }

    // Nothing ran.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT count FROM Widget ORDER BY id"
        ),
        vec![vec!["3"], vec!["1"], vec!["2"]]
    );
}

#[test]
fn describing_leaves_the_session_alone() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "INSERT INTO Widget (id, name) VALUES ('d', 'delta')",
    );
    let affected_before = session.affected_rows;

    engine
        .describe(&session, "SELECT id FROM Widget", 0)
        .unwrap();
    assert_eq!(session.affected_rows, affected_before);
}

// --- JSON -------------------------------------------------------------------

#[test]
fn json_paths_extract_and_unquote() {
    let (engine, mut session) = engine();
    let doc = r#"{"id": 7, "tags": ["a", "b"], "label": "seven", "nested": {"k": 1}}"#;

    assert_eq!(
        rows(
            &engine,
            &mut session,
            &format!(
                "SELECT JSON_EXTRACT('{doc}', '$.id'), \
                        JSON_EXTRACT('{doc}', '$.label'), \
                        JSON_UNQUOTE(JSON_EXTRACT('{doc}', '$.label')), \
                        JSON_EXTRACT('{doc}', '$.tags[1]'), \
                        JSON_EXTRACT('{doc}', '$.nested.k'), \
                        JSON_EXTRACT('{doc}', '$.missing')"
            )
        ),
        vec![vec!["7", "\"seven\"", "seven", "\"b\"", "1", "NULL"]]
    );

    // A wildcard selects many, so the result is an array even when it matches
    // once — that is how a caller can tell the two apart.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            &format!("SELECT JSON_EXTRACT('{doc}', '$.tags[*]')")
        ),
        vec![vec!["[\"a\", \"b\"]"]]
    );
}

#[test]
fn the_arrow_operators_are_extract_and_unquote() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Doc (id INTEGER NOT NULL, body JSON NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        r#"INSERT INTO Doc VALUES (1, '{"name": "ada", "n": 3}')"#,
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT body -> '$.name', body ->> '$.name', body -> '$.n' FROM Doc"
        ),
        vec![vec!["\"ada\"", "ada", "3"]]
    );
}

#[test]
fn json_documents_are_built_and_merged() {
    let (engine, mut session) = engine();

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_OBJECT('a', 1, 'b', 'two'), JSON_ARRAY(1, 'x', NULL)"
        ),
        vec![vec!["{\"a\": 1, \"b\": \"two\"}", "[1, \"x\", null]"]]
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT JSON_MERGE_PATCH('{"a":1,"b":2}', '{"b":3,"c":4}'),
                      JSON_SET('{"a":1}', '$.a', 9, '$.b', 2),
                      JSON_ARRAY_APPEND('{"xs":[1]}', '$.xs', 2)"#
        ),
        vec![vec![
            "{\"a\": 1, \"b\": 3, \"c\": 4}",
            "{\"a\": 9, \"b\": 2}",
            "{\"xs\": [1, 2]}"
        ]]
    );

    // Null in a patch removes the member rather than setting it to null.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT JSON_MERGE_PATCH('{"a":1,"b":2}', '{"b":null}')"#
        ),
        vec![vec!["{\"a\": 1}"]]
    );
}

#[test]
fn json_is_inspected_and_validated() {
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT JSON_VALID('{"a":1}'), JSON_VALID('{oops'),
                      JSON_TYPE('[]'), JSON_TYPE('"s"'), JSON_TYPE('1.5'),
                      JSON_LENGTH('[1,2,3]'), JSON_KEYS('{"a":1,"b":2}'),
                      JSON_CONTAINS('{"a":1,"b":2}', '{"a":1}')"#
        ),
        vec![vec![
            "1",
            "0",
            "ARRAY",
            "STRING",
            "DOUBLE",
            "3",
            "[\"a\", \"b\"]",
            "1"
        ]]
    );

    // Text that is not JSON is an error, not a NULL: a column holding a
    // truncated document is a bug, and NULL hides it as merely absent.
    let err = engine
        .execute(&mut session, "SELECT JSON_EXTRACT('{oops', '$.a')", &[])
        .unwrap_err();
    assert_eq!(err.code, 3141);

    // So is a path the engine cannot read.
    let err = engine
        .execute(&mut session, "SELECT JSON_EXTRACT('{}', 'a.b')", &[])
        .unwrap_err();
    assert_eq!(err.code, 3143);
}

#[test]
fn json_arrayagg_keeps_each_value_s_type() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAYAGG(count) FROM Widget"
        ),
        vec![vec!["[3, 1, 2]"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAYAGG(name) FROM Widget WHERE count > 1"
        ),
        vec![vec!["[\"alpha\", \"gamma\"]"]]
    );
}

#[test]
fn json_table_turns_a_document_into_rows() {
    let (engine, mut session) = engine();

    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT jt.n, jt.id, jt.label
               FROM JSON_TABLE(
                   '[{"id": "a", "label": "first"}, {"id": "b"}]',
                   '$[*]' COLUMNS(
                       n FOR ORDINALITY,
                       id VARCHAR(10) PATH '$.id',
                       label VARCHAR(20) PATH '$.label'
                   )
               ) jt"#
        ),
        // A path that matches nothing is NULL, which is the only sensible
        // reading of "this element has no such member".
        vec![vec!["1", "a", "first"], vec!["2", "b", "NULL"]]
    );
}

#[test]
fn json_table_reads_the_document_from_the_row_it_is_joined_to() {
    // JSON_TABLE is implicitly LATERAL: the document is a column of a table
    // named before it, so it expands once per row rather than once per query.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Board (id INTEGER NOT NULL, config JSON NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        r#"INSERT INTO Board VALUES
            (1, '[{"col": "one"}, {"col": "two"}]'),
            (2, '[{"col": "three"}]')"#,
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT b.id, jt.col FROM Board b, \
             JSON_TABLE(b.config, '$[*]' COLUMNS(col VARCHAR(10) PATH '$.col')) jt \
             ORDER BY b.id, jt.col",
        ),
        vec![vec!["1", "one"], vec!["1", "two"], vec!["2", "three"]]
    );

    // And from the enclosing query when it is the only thing in a subquery's
    // FROM, which is how the migrations rewrite a stored configuration.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, (SELECT COUNT(*) FROM \
             JSON_TABLE(Board.config, '$[*]' COLUMNS(col VARCHAR(10) PATH '$.col')) jt) \
             FROM Board ORDER BY id",
        ),
        vec![vec!["1", "2"], vec!["2", "1"]]
    );
}

#[test]
fn on_duplicate_key_update_updates_instead_of_failing() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Pair (
            id INTEGER NOT NULL,
            a INTEGER NOT NULL,
            b INTEGER NOT NULL,
            note VARCHAR(20) NULL,
            PRIMARY KEY (id),
            UNIQUE INDEX Pair_a_b_key (a, b)
        )",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Pair VALUES (1, 1, 1, 'first')",
    );

    // A collision on the composite key updates the row that is already there.
    // MySQL reports two affected rows for that, and one for an insert.
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "INSERT INTO Pair VALUES (2, 1, 1, 'second') ON DUPLICATE KEY UPDATE note = 'updated'"
        ),
        2
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, note FROM Pair ORDER BY id"
        ),
        vec![vec!["1", "updated"]]
    );

    assert_eq!(
        affected(
            &engine,
            &mut session,
            "INSERT INTO Pair VALUES (3, 2, 2, 'new') ON DUPLICATE KEY UPDATE note = 'updated'"
        ),
        1
    );

    // The right-hand side sees the row as it stands, so it can build on it.
    run(
        &engine,
        &mut session,
        "INSERT INTO Pair VALUES (4, 2, 2, 'x') ON DUPLICATE KEY UPDATE a = a + 10",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT a FROM Pair WHERE id = 3"),
        vec![vec!["12"]]
    );
}

#[test]
fn a_case_insensitive_collate_is_the_comparison_the_engine_already_does() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM Widget WHERE name COLLATE utf8mb4_unicode_ci = 'ALPHA'"
        ),
        vec![vec!["a"]]
    );

    // A collation that would compare differently is refused rather than
    // quietly ignored, which would answer the wrong question.
    let err = engine
        .execute(
            &mut session,
            "SELECT id FROM Widget WHERE name COLLATE utf8mb4_bin = 'ALPHA'",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1235);
}

#[test]
fn a_lateral_join_honours_its_on_clause() {
    // Reading the operator only for its outer-ness turned every
    // `JOIN JSON_TABLE(...) ON ...` into a cross join, silently.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Doc2 (id INTEGER NOT NULL, doc JSON NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        r#"INSERT INTO Doc2 VALUES (1, '[{"k":1},{"k":2},{"k":3}]')"#,
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT t.id, jt.k FROM Doc2 t \
             JOIN JSON_TABLE(t.doc, '$[*]' COLUMNS(k INT PATH '$.k')) jt ON jt.k = 2"
        ),
        vec![vec!["1", "2"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT t.id FROM Doc2 t \
             JOIN JSON_TABLE(t.doc, '$[*]' COLUMNS(k INT PATH '$.k')) jt ON 1 = 0"
        ),
        Vec::<Vec<String>>::new()
    );

    // A left join keeps the row and pads it, rather than keeping every
    // unmatched expansion.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT t.id, jt.k FROM Doc2 t \
             LEFT JOIN JSON_TABLE(t.doc, '$[*]' COLUMNS(k INT PATH '$.k')) jt ON jt.k = 99"
        ),
        vec![vec!["1", "NULL"]]
    );

    // A join type the ordinary path refuses must not be quietly downgraded to
    // an inner join here.
    let err = engine
        .execute(
            &mut session,
            "SELECT t.id FROM Doc2 t \
             RIGHT JOIN JSON_TABLE(t.doc, '$[*]' COLUMNS(k INT PATH '$.k')) jt ON jt.k = 1",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1235);
}

#[test]
fn json_extract_is_null_when_no_path_matches() {
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT JSON_EXTRACT('{"a":1}', '$.x', '$.y'), JSON_EXTRACT('[]', '$[*]')"#
        ),
        vec![vec!["NULL", "NULL"]]
    );
}

#[test]
fn a_wildcard_wraps_a_document_that_is_not_an_array() {
    // MySQL treats a scalar or object as a one-element array for both `[0]`
    // and `[*]`. Without it a JSON_TABLE over an object yields no rows and the
    // row disappears from the join.
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT JSON_EXTRACT('{"a":1}', '$[*]'), JSON_EXTRACT('7', '$[*]')"#
        ),
        vec![vec!["[{\"a\": 1}]", "[7]"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT jt.a FROM JSON_TABLE('{"a":5}', '$[*]' COLUMNS(a INT PATH '$.a')) jt"#
        ),
        vec![vec!["5"]]
    );
}

#[test]
fn a_json_null_is_a_value_not_an_absence() {
    // "The key holds null" and "there is no such key" are different answers,
    // and collapsing them makes IS NULL true for both.
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT JSON_EXTRACT('{"a":null}', '$.a') IS NULL,
                      JSON_EXTRACT('{"a":null}', '$.b') IS NULL,
                      JSON_UNQUOTE(JSON_EXTRACT('{"a":null}', '$.a')),
                      JSON_TYPE(JSON_EXTRACT('{"a":null}', '$.a'))"#
        ),
        vec![vec!["0", "1", "null", "NULL"]]
    );
}

#[test]
fn a_json_path_may_be_spaced_the_way_mysql_allows() {
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT JSON_EXTRACT('{"a":1}', '$. a'),
                      JSON_EXTRACT('{"a":{"b":2}}', '$.a .b'),
                      JSON_EXTRACT('{"a":1}', '$ .a')"#
        ),
        vec![vec!["1", "2", "1"]]
    );
}

#[test]
fn json_arrayagg_keeps_nulls_and_is_null_when_empty() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE N2 (id INTEGER NOT NULL, v INTEGER NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO N2 VALUES (1,1),(2,NULL),(3,2)",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT JSON_ARRAYAGG(v) FROM N2"),
        vec![vec!["[1, null, 2]"]]
    );
    // No rows is NULL, not an empty array.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAYAGG(v) FROM N2 WHERE v > 99"
        ),
        vec![vec!["NULL"]]
    );
    // MySQL has no DISTINCT here, so applying one would answer a question it
    // never accepts.
    let err = engine
        .execute(
            &mut session,
            "SELECT JSON_ARRAYAGG(DISTINCT v) FROM N2",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1235);
}

#[test]
fn a_json_table_column_clause_that_changes_the_answer_is_refused() {
    let (engine, mut session) = engine();
    for sql in [
        "SELECT jt.k FROM JSON_TABLE('[{}]', '$[*]' COLUMNS(k INT PATH '$.k' DEFAULT '9' ON EMPTY)) jt",
        "SELECT jt.k FROM JSON_TABLE('[{}]', '$[*]' COLUMNS(k INT PATH '$.k' ERROR ON EMPTY)) jt",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1235, "for: {sql}");
    }

    // A column holds one value, so a path that can select several is refused
    // rather than silently picking.
    let err = engine
        .execute(
            &mut session,
            "SELECT jt.k FROM JSON_TABLE('[[1,2]]', '$[*]' COLUMNS(k INT PATH '$[*]')) jt",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 3153);
}

#[test]
fn an_upsert_reports_no_insert_id_and_takes_the_values_it_would_have_written() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Up (
            id INTEGER NOT NULL AUTO_INCREMENT,
            k VARCHAR(10) NOT NULL,
            n INTEGER NOT NULL,
            PRIMARY KEY (id),
            UNIQUE INDEX Up_k_key (k)
        )",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Up (k, n) VALUES ('a', 1)",
    );
    let first_id = rows(&engine, &mut session, "SELECT LAST_INSERT_ID()");

    // `VALUES(n)` is the ordinary way to spell "take the new value".
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "INSERT INTO Up (k, n) VALUES ('a', 5) ON DUPLICATE KEY UPDATE n = VALUES(n)"
        ),
        2
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT n FROM Up WHERE k = 'a'"),
        vec![vec!["5"]]
    );

    // The counter is consumed, as MySQL does — but no id is reported for a row
    // that was never inserted, or a client reads back a phantom.
    assert_eq!(
        rows(&engine, &mut session, "SELECT LAST_INSERT_ID()"),
        first_id
    );
}

#[test]
fn an_upsert_updates_the_row_the_primary_key_names() {
    // Colliding on both the primary key and a secondary one, MySQL updates the
    // row the primary key found.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Two (
            id INTEGER NOT NULL,
            k VARCHAR(10) NOT NULL,
            n INTEGER NOT NULL,
            PRIMARY KEY (id),
            UNIQUE INDEX Two_k_key (k)
        )",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Two VALUES (1,'a',3), (4,'b',1)",
    );

    run(
        &engine,
        &mut session,
        "INSERT INTO Two VALUES (1,'b',1) ON DUPLICATE KEY UPDATE n = 999",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id, n FROM Two ORDER BY id"),
        vec![vec!["1", "999"], vec!["4", "1"]]
    );
}

#[test]
fn a_binary_string_is_read_for_its_leading_number() {
    // MySQL reads a string, binary or not, for the number at the front of it.
    // Treating a byte string as "true when non-empty" made `WHERE BINARY x`
    // true for the byte '0' — so a filter over a cast comparison kept every
    // row, which is how a foreign-key lookup came back as a cross join.
    let (engine, mut session) = engine();

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST('a' = 'b' AS BINARY), CAST('a' = 'a' AS BINARY)"
        ),
        vec![vec!["0", "1"]]
    );

    widgets(&engine, &mut session);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM Widget WHERE CAST(name = 'alpha' AS BINARY)"
        ),
        vec![vec!["a"]]
    );

    // The same rule for ordinary strings: the leading number decides.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT IF('0', 'yes', 'no'), IF('1abc', 'yes', 'no'), IF('abc', 'yes', 'no')"
        ),
        vec![vec!["no", "yes", "no"]]
    );
}

#[test]
fn a_view_column_is_described_like_a_table_column() {
    // A view's columns carry real types, and describing them as type-less made
    // Prisma's introspection unwrap a None — `db pull` against any schema with
    // a view crashed.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Src (
            id INTEGER NOT NULL,
            label VARCHAR(40) NOT NULL,
            at DATE NOT NULL,
            PRIMARY KEY (id)
        )",
    );
    run(
        &engine,
        &mut session,
        "CREATE VIEW Seen AS SELECT id, label, at FROM Src",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION, DATETIME_PRECISION \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'Seen' ORDER BY ORDINAL_POSITION"
        ),
        vec![
            // Exact, not widened: the declared type travels with the column
            // now, so `int` is 10 digits rather than the 19 a wire `LONGLONG`
            // implies and `varchar(40)` keeps its 40 rather than reading as
            // text.
            vec!["id", "NULL", "10", "NULL"],
            vec!["label", "40", "NULL", "NULL"],
            // A DATE has no fractional seconds, which MySQL reports as 0
            // rather than NULL: the precision exists and it is zero.
            vec!["at", "NULL", "NULL", "0"],
        ]
    );
}

#[test]
fn an_aggregate_keeps_the_type_of_what_it_folds() {
    // Declared as text, `SUM(views)` reaches a typed client as the string
    // "120" where MySQL sends the number.
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    let described = engine
        .describe(
            &session,
            "SELECT COUNT(*), SUM(count), MAX(count), MIN(name), AVG(count) FROM Widget",
            0,
        )
        .unwrap();
    let types: Vec<String> = described.iter().map(|c| format!("{:?}", c.ty)).collect();
    // MIN and MAX keep the argument's type, so folding an INTEGER gives
    // MySQL's `LONG` rather than the `LONGLONG` every integer used to be. SUM
    // and AVG do not: a total outgrows what it totals, which is why MySQL
    // answers NEWDECIMAL and why announcing the argument's width would have
    // the binary protocol send too few bytes for it.
    assert_eq!(
        types,
        vec!["LongLong", "NewDecimal", "Long", "VarString", "NewDecimal"],
        "an aggregate should carry its argument's type"
    );
}

#[test]
fn now_honours_its_precision() {
    // A `DATETIME(3)` column filled by `NOW(3)` holding whole seconds makes two
    // rows written in the same second compare equal.
    let (engine, mut session) = engine();
    let stamped = rows(&engine, &mut session, "SELECT NOW(3), NOW(6), NOW()");

    assert!(
        stamped[0][0].len() == 23 && stamped[0][0].contains('.'),
        "NOW(3) gave {}",
        stamped[0][0]
    );
    assert_eq!(stamped[0][1].len(), 26, "NOW(6) gave {}", stamped[0][1]);
    assert_eq!(stamped[0][2].len(), 19, "NOW() gave {}", stamped[0][2]);
}

#[test]
fn information_schema_reports_numbers_as_numbers() {
    // These columns are read by tools, not people. Prisma's schema describer
    // asks `statistics` for `seq_in_index` and unwraps it as an integer, so
    // handing back the string "1" crashed `prisma db pull` outright.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Typed (
            id INTEGER NOT NULL AUTO_INCREMENT,
            note VARCHAR(20) NULL,
            body TEXT NULL,
            kind ENUM('a', 'bb', 'ccc') NULL,
            amount DECIMAL(10, 2) NULL,
            at DATETIME(3) NULL,
            PRIMARY KEY (id)
        )",
    );

    let ordinal = rows(
        &engine,
        &mut session,
        "SELECT SEQ_IN_INDEX + 1 FROM information_schema.statistics \
         WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'Typed'",
    );
    assert_eq!(ordinal, vec![vec!["2"]], "seq_in_index is not arithmetic");

    // MySQL reports a precision for every numeric type and a maximum length
    // for every string type, whether or not the DDL wrote one.
    let described = rows(
        &engine,
        &mut session,
        "SELECT COLUMN_NAME, CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION, NUMERIC_SCALE, \
                DATETIME_PRECISION \
         FROM information_schema.columns \
         WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'Typed' ORDER BY ORDINAL_POSITION",
    );
    assert_eq!(
        described,
        vec![
            vec!["id", "NULL", "10", "0", "NULL"],
            vec!["note", "20", "NULL", "NULL", "NULL"],
            vec!["body", "65535", "NULL", "NULL", "NULL"],
            // An ENUM's maximum is its longest member.
            vec!["kind", "3", "NULL", "NULL", "NULL"],
            vec!["amount", "NULL", "10", "2", "NULL"],
            vec!["at", "NULL", "NULL", "NULL", "3"],
        ]
    );
}

// --- dates, times and geography ---------------------------------------------

#[test]
fn the_date_parts_come_out() {
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT YEAR('2026-05-02 10:20:30'), MONTH('2026-05-02 10:20:30'),
                    DAY('2026-05-02 10:20:30'), HOUR('2026-05-02 10:20:30'),
                    MINUTE('2026-05-02 10:20:30'), SECOND('2026-05-02 10:20:30'),
                    QUARTER('2026-05-02'), DAYOFYEAR('2026-05-02'),
                    DAYOFWEEK('2026-05-02'), WEEKDAY('2026-05-02')"
        ),
        // 2 May 2026 is a Saturday: DAYOFWEEK counts from Sunday = 1, WEEKDAY
        // from Monday = 0, and the two disagree on purpose.
        vec![vec![
            "2026", "5", "2", "10", "20", "30", "2", "122", "7", "5"
        ]]
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT HOUR(NULL), DAYOFWEEK(NULL)"),
        vec![vec!["NULL", "NULL"]]
    );
}

#[test]
fn date_format_renders_the_specifiers_it_claims() {
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r"SELECT DATE_FORMAT('2026-01-02 03:04:05', '%Y-%m'),
                     DATE_FORMAT('2026-01-02 03:04:05', '%Y-%m-%dT%H:%i:%s.000Z'),
                     DATE_FORMAT('2026-01-02 15:04:05', '%W %M %e, %Y at %h:%i %p'),
                     DATE_FORMAT('2026-01-02', '100%% of %j')"
        ),
        vec![vec![
            "2026-01",
            "2026-01-02T03:04:05.000Z",
            "Friday January 2, 2026 at 03:04 PM",
            "100% of 002"
        ]]
    );

    // A specifier the engine has not implemented is an error, not passed
    // through: MySQL renders an unknown one as the bare letter, so a silent
    // pass-through would be indistinguishable from a literal and the caller
    // would get a string that looks almost right.
    let err = engine
        .execute(&mut session, "SELECT DATE_FORMAT('2026-01-02', '%U')", &[])
        .unwrap_err();
    assert_eq!(err.code, 1235);
}

#[test]
fn date_arithmetic_walks_the_calendar() {
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DATE_ADD('2026-01-31', INTERVAL 1 DAY),
                    DATE_SUB('2026-03-01', INTERVAL 1 DAY),
                    DATE_ADD('2024-02-28', INTERVAL 1 DAY),
                    DATE_ADD('2026-01-31', INTERVAL 90 MINUTE),
                    DATE_ADD('2026-01-01', INTERVAL 1 YEAR),
                    ADDDATE('2026-01-01', 7)"
        ),
        vec![vec![
            "2026-02-01",
            "2026-02-28",
            // 2024 is a leap year, so the 28th is not the last day.
            "2024-02-29",
            // A unit with a clock turns a date into a datetime.
            "2026-01-31 01:30:00",
            "2027-01-01",
            "2026-01-08"
        ]]
    );

    // Adding a month clamps to the shorter month rather than rolling over,
    // which is what MySQL does and what makes month arithmetic non-obvious.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DATE_ADD('2026-01-31', INTERVAL 1 MONTH),
                    DATE_ADD('2026-01-31', INTERVAL 13 MONTH),
                    DATE_SUB('2026-03-31', INTERVAL 1 MONTH)"
        ),
        vec![vec!["2026-02-28", "2027-02-28", "2026-02-28"]]
    );
}

#[test]
fn differences_are_measured_in_whole_units() {
    let (engine, mut session) = engine();

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DATEDIFF('2026-01-10', '2026-01-01'),
                    DATEDIFF('2026-01-01', '2026-01-10'),
                    DATEDIFF('2026-01-02 23:59:59', '2026-01-01 00:00:01')"
        ),
        // DATEDIFF counts days, not 24-hour spans: the third is 1, not 1.99.
        vec![vec!["9", "-9", "1"]]
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TIMESTAMPDIFF(SECOND, '2026-01-01 00:00:00', '2026-01-01 00:02:03'),
                    TIMESTAMPDIFF(MINUTE, '2026-01-01 00:00:00', '2026-01-01 02:00:00'),
                    TIMESTAMPDIFF(HOUR, '2026-01-01 00:00:00', '2026-01-01 02:59:00'),
                    TIMESTAMPDIFF(DAY, '2026-01-01', '2026-01-10'),
                    TIMESTAMPDIFF(MONTH, '2026-01-31', '2026-03-01'),
                    TIMESTAMPDIFF(YEAR, '2026-01-01', '2027-06-01')"
        ),
        // Truncated toward zero throughout: 2h59m is two hours, and 31 Jan to
        // 1 Mar is one month because the day of month has not come round yet.
        vec![vec!["123", "120", "2", "9", "1", "1"]]
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DATEDIFF(NULL, '2026-01-01'), TIMESTAMPDIFF(DAY, NULL, '2026-01-01')"
        ),
        vec![vec!["NULL", "NULL"]]
    );
}

#[test]
fn a_malformed_date_is_null_not_a_nearby_one() {
    // MySQL answers NULL with a warning for a bad datetime *inside an
    // expression* — strict mode governs data-change statements, not
    // expression evaluation — so failing the statement here would kill a
    // query MySQL answers. What must not happen is a nearby date, which would
    // hide the bad value instead of showing it.
    let (engine, mut session) = engine();
    for sql in [
        "SELECT DATEDIFF('2026-13-01', '2026-01-01')",
        "SELECT DATE_ADD('not a date', INTERVAL 1 DAY)",
        "SELECT HOUR('2026-01-01 25:00:00')",
        "SELECT DATE_FORMAT('2026-02-30', '%Y')",
        // Day zero is not the last of the previous month, and reading it as
        // 2025-12-31 would answer a different question without saying so.
        "SELECT DATE_ADD('2026-01-00', INTERVAL 1 DAY)",
        "SELECT DAYOFYEAR('2026-01-00')",
        "SELECT DATE_FORMAT('2026-01-00', '%M')",
        "SELECT DATE_ADD('2026-00-01', INTERVAL 1 DAY)",
        // Out of range rather than merely wrong: parsed as `i64` and checked
        // before narrowing, because `as u32` wraps this one to day zero and it
        // would then take the path above and answer.
        "SELECT DATE_ADD('2026-01-4294967296', INTERVAL 1 DAY)",
        // The year is bounded to MySQL's own range at both ends.
        "SELECT DATE_ADD('99999-01-01', INTERVAL 1 DAY)",
        "SELECT DATE_ADD('9999-12-31', INTERVAL 1 DAY)",
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec!["NULL"]],
            "{sql}"
        );
    }

    // One bad value behaves one way. These used to split on '-' and read the
    // year out of a date that has no year, while QUARTER of the same text was
    // NULL.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT YEAR('2026-13-45'), MONTH('2026-13-45'), DAY('2026-13-45'), QUARTER('2026-13-45')"
        ),
        vec![vec!["NULL", "NULL", "NULL", "NULL"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT YEAR('2026-05-02'), MONTH('2026-05-02 11:00:00'), DAY('2026-05-02')"
        ),
        vec![vec!["2026", "5", "2"]]
    );

    // An interval the engine cannot read is still a clear error, because that
    // is a gap in the engine rather than a value MySQL would call NULL.
    //
    // The compound unit is refused by the parser before the engine sees it,
    // which is the same outcome by a shorter route.
    let err = engine
        .execute(
            &mut session,
            "SELECT DATE_ADD('2026-01-01', INTERVAL '1 2' DAY_HOUR)",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1064);

    let err = engine
        .execute(
            &mut session,
            "SELECT TIMESTAMPDIFF(FORTNIGHT, '2026-01-01', '2026-02-01')",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1235);

    // The boundaries the day check is built on still pass on both sides.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DATE_ADD('2024-02-29', INTERVAL 1 DAY), DATE_ADD('2026-01-31', INTERVAL 1 DAY)"
        ),
        vec![vec!["2024-03-01", "2026-02-01"]]
    );
}

#[test]
fn distance_on_the_sphere_matches_mysql() {
    let (engine, mut session) = engine();

    // One degree of latitude on MySQL's sphere, whose radius is 6,370,986m.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT ROUND(ST_Distance_Sphere(POINT(0, 0), POINT(0, 1))),
                    ROUND(ST_Distance_Sphere(POINT(0, 0), POINT(0, 0)))"
        ),
        vec![vec!["111195", "0"]]
    );

    // A point is held as WKT, which is what ST_AsText would return.
    assert_eq!(
        rows(&engine, &mut session, "SELECT POINT(1.5, -2.25)"),
        vec![vec!["POINT(1.5 -2.25)"]]
    );

    // The bounds are MySQL's, and the asymmetry is real: longitude is
    // exclusive at -180 and inclusive at 180.
    let err = engine
        .execute(
            &mut session,
            "SELECT ST_Distance_Sphere(POINT(-180, 0), POINT(0, 0))",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 3616);

    run(
        &engine,
        &mut session,
        "SELECT ST_Distance_Sphere(POINT(180, 0), POINT(0, 0))",
    );

    let err = engine
        .execute(
            &mut session,
            "SELECT ST_Distance_Sphere(POINT(0, 91), POINT(0, 0))",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 3617);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT ST_Distance_Sphere(NULL, POINT(0, 0))"
        ),
        vec![vec!["NULL"]]
    );

    // Antipodal points are half the circumference apart, not NaN. The
    // haversine term is at most 1 in exact arithmetic but can round a couple
    // of ulps past it, and `asin` above 1 is NaN — so the third pair here,
    // found by sweeping near-antipodal points for one whose `sqrt` stays above
    // 1, is the case the clamp exists for.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT ROUND(ST_Distance_Sphere(POINT(0, 90), POINT(0, -90))),
                    ROUND(ST_Distance_Sphere(POINT(0, 0), POINT(180, 0))),
                    ROUND(ST_Distance_Sphere(
                        POINT(15.029258415716725, -62.926012829802886),
                        POINT(-164.9707415845351, 62.92601282966185)))"
        ),
        vec![vec!["20015043", "20015043", "20015043"]]
    );
}

#[test]
fn an_aggregate_with_no_from_folds_over_one_implicit_row() {
    let (engine, mut session) = engine();

    // A FROM-less SELECT is evaluated against one implicit row, not none, so
    // the argument is folded rather than skipped. Over an empty group these
    // would be `0, NULL, NULL`, which reads as an answer rather than a bug.
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*), SUM(1), MAX(2)"),
        vec![vec!["1", "1", "2"]]
    );

    // The implicit row is the enclosing one where there is one, so a
    // correlated aggregate still counts its own row rather than zero.
    run(&engine, &mut session, "CREATE TABLE Two (id INT)");
    run(&engine, &mut session, "INSERT INTO Two VALUES (1), (2)");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, (SELECT COUNT(*)) FROM Two"
        ),
        vec![vec!["1", "1"], vec!["2", "1"]]
    );
}

#[test]
fn an_interval_too_large_to_land_is_null_rather_than_a_panic() {
    let (engine, mut session) = engine();

    // Every one of these is a statement anyone can write, and each multiplies
    // the amount by something. Unchecked they panic in a debug build and wrap
    // to a plausible-looking wrong date in release, where overflow checks are
    // off — so the failure would reach a user as an answer.
    for sql in [
        "SELECT DATE_ADD('2026-01-01', INTERVAL 999999999999999999 DAY)",
        "SELECT DATE_ADD('2026-01-01', INTERVAL 9223372036854775807 YEAR)",
        "SELECT DATE_ADD('2026-01-01', INTERVAL 9223372036854775807 QUARTER)",
        "SELECT DATE_ADD('2026-01-01', INTERVAL 9223372036854775807 WEEK)",
        "SELECT DATE_ADD('2026-01-01', INTERVAL 9223372036854775807 HOUR)",
        // Negating i64::MIN overflows, which is its own arithmetic trap.
        "SELECT DATE_SUB('2026-01-01', INTERVAL -9223372036854775808 DAY)",
        "SELECT TIMESTAMPADD(YEAR, 9223372036854775807, '2026-01-01')",
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec!["NULL"]],
            "{sql}"
        );
    }

    // The widest span that does fit is still computed rather than refused.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TIMESTAMPDIFF(MICROSECOND, '0000-01-01', '9999-12-31')"
        ),
        vec![vec!["315569433600000000"]]
    );
}

#[test]
fn an_interval_reads_null_and_fractions_as_mysql_does() {
    let (engine, mut session) = engine();

    // NULL is a value here, not a failure: this is the shape a prepared
    // `DATE_SUB(NOW(), INTERVAL ? DAY)` takes when the parameter binds NULL,
    // and erroring would fail the whole statement over one row.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DATE_ADD('2026-01-01', INTERVAL NULL DAY), DATE_SUB(NULL, INTERVAL 1 DAY)"
        ),
        vec![vec!["NULL", "NULL"]]
    );

    // A fractional amount rounds rather than truncating, so 1.5 days is two.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DATE_ADD('2026-01-01', INTERVAL 1.5 DAY),
                    DATE_ADD('2026-01-01', INTERVAL 1.2 DAY)"
        ),
        vec![vec!["2026-01-03", "2026-01-02"]]
    );
}

#[test]
fn a_bare_interval_unit_is_not_read_as_a_column() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Span (id INT, d DATETIME)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Span VALUES (1, '2026-01-01 00:00:00'), (1, '2026-03-05 00:00:00')",
    );

    // TIMESTAMPDIFF writes its unit as a bare keyword, which parses as an
    // identifier indistinguishable from a column. The ONLY_FULL_GROUP_BY check
    // walks the raw AST, so without dropping that argument it rejects this for
    // selecting a nonaggregated column named `DAY`.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, TIMESTAMPDIFF(DAY, MIN(d), MAX(d)) FROM Span GROUP BY id"
        ),
        vec![vec!["1", "63"]]
    );

    // A genuinely ungrouped column in the same call is still caught.
    let err = engine
        .execute(
            &mut session,
            "SELECT id, TIMESTAMPDIFF(DAY, d, MAX(d)) FROM Span GROUP BY id",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1055);
}

#[test]
fn the_clock_extractors_take_a_bare_time() {
    let (engine, mut session) = engine();

    // HOUR, MINUTE, SECOND and MICROSECOND accept a TIME, which has no
    // calendar at all — a `WHERE HOUR(t) = 9` over a TIME column would
    // otherwise fail the query rather than filter it.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT HOUR('10:30:45'), MINUTE('10:30:45'), SECOND('10:30:45'),
                    MICROSECOND('10:30:45.5')"
        ),
        vec![vec!["10", "30", "45", "500000"]]
    );

    // A TIME runs past 24 hours, because it is a duration as much as a clock.
    assert_eq!(
        rows(&engine, &mut session, "SELECT HOUR('838:59:59')"),
        vec![vec!["838"]]
    );

    // The extractors that need a calendar do not accept one.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DAYOFWEEK('10:30:45'), QUARTER('10:30:45')"
        ),
        vec![vec!["NULL", "NULL"]]
    );
}

#[test]
fn a_fraction_is_rendered_at_full_width() {
    let (engine, mut session) = engine();

    // MySQL renders `.100000`, not `.1`. Trimming would give a different
    // string for the same instant than DATE_FORMAT's `%f`, and a client
    // reading fractional seconds by fixed width would misread it.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DATE_ADD('2026-01-01 00:00:00.1', INTERVAL 1 SECOND),
                    DATE_FORMAT('2026-01-01 00:00:00.1', '%f')"
        ),
        vec![vec!["2026-01-01 00:00:01.100000", "100000"]]
    );
}

#[test]
fn limit_narrows_an_update_or_delete_to_the_rows_order_by_picked() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    // Storage order is a, b, c and the counts are 3, 1, 2 — so every pick
    // below lands on a different row than "the first one stored", which is
    // what the statement would touch if the ORDER BY were dropped.
    run(
        &engine,
        &mut session,
        "DELETE FROM Widget ORDER BY count ASC LIMIT 1",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, count FROM Widget ORDER BY id"
        ),
        vec![vec!["a", "3"], vec!["c", "2"]]
    );

    // Ascending again, so the lower of the two remaining counts changes and
    // the first row stored does not.
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "UPDATE Widget SET count = 99 ORDER BY count ASC LIMIT 1"
        ),
        1
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, count FROM Widget ORDER BY id"
        ),
        vec![vec!["a", "3"], vec!["c", "99"]]
    );

    // The WHERE clause narrows first and the LIMIT second, so a LIMIT larger
    // than the match count takes all of it.
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "DELETE FROM Widget WHERE count > 50 ORDER BY id LIMIT 5"
        ),
        1
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget ORDER BY id"),
        vec![vec!["a"]]
    );

    // LIMIT 0 is a statement that matches rows and touches none of them.
    assert_eq!(
        affected(&engine, &mut session, "UPDATE Widget SET count = 7 LIMIT 0"),
        0
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT count FROM Widget"),
        vec![vec!["3"]]
    );
}

#[test]
fn a_limit_is_an_expression_and_may_be_a_parameter() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "DELETE FROM Widget ORDER BY id LIMIT 1 + 1",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget"),
        vec![vec!["c"]]
    );

    // A bound parameter, which is the shape an application actually sends.
    let (engine, mut session) = fresh_engine();
    widgets(&engine, &mut session);
    let outcome = engine
        .execute(
            &mut session,
            "DELETE FROM Widget ORDER BY id LIMIT ?",
            &[Value::Int(2)],
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Affected { rows: 2, .. }));
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget"),
        vec![vec!["c"]]
    );
}

#[test]
fn order_by_sets_the_order_an_update_writes_in() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Seq (id INT NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Seq VALUES (1), (2), (3)",
    );

    // Shifting every key up by one collides with itself unless the rows are
    // written from the top down — which is the whole reason ORDER BY is
    // honoured even with no LIMIT to narrow anything.
    let err = engine
        .execute(&mut session, "UPDATE Seq SET id = id + 1", &[])
        .unwrap_err();
    assert_eq!(err.code, 1062);

    let err = engine
        .execute(
            &mut session,
            "UPDATE Seq SET id = id + 1 ORDER BY id ASC",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1062);

    assert_eq!(
        affected(
            &engine,
            &mut session,
            "UPDATE Seq SET id = id + 1 ORDER BY id DESC"
        ),
        3
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Seq ORDER BY id"),
        vec![vec!["2"], vec!["3"], vec!["4"]]
    );
}

#[test]
fn delete_using_names_its_targets_after_the_from() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Tag (wid VARCHAR(21), label VARCHAR(20))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Tag VALUES ('a', 'keep'), ('b', 'drop'), ('c', 'drop')",
    );

    // MySQL's other multi-table spelling: the targets come after FROM and the
    // join comes after USING, where `DELETE a FROM a JOIN b` puts them the
    // other way round. Both mean the same thing.
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "DELETE FROM Widget USING Widget JOIN Tag ON Tag.wid = Widget.id \
             WHERE Tag.label = 'drop'"
        ),
        2
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget"),
        vec![vec!["a"]]
    );
    // Only the named target lost rows; the joined table is untouched.
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Tag"),
        vec![vec!["3"]]
    );
}

#[test]
fn delete_using_takes_several_targets_and_aliases() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Tag (wid VARCHAR(21), label VARCHAR(20))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Tag VALUES ('a', 'keep'), ('b', 'drop'), ('c', 'drop')",
    );

    // Two targets, so both tables lose the rows the join matched: two widgets
    // and two tags.
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "DELETE FROM Widget, Tag USING Widget JOIN Tag ON Tag.wid = Widget.id \
             WHERE Tag.label = 'drop'"
        ),
        4
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget"),
        vec![vec!["a"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT wid FROM Tag"),
        vec![vec!["a"]]
    );

    // A target is addressed by its alias, because that is what USING called it.
    let (engine, mut session) = fresh_engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Tag (wid VARCHAR(21), label VARCHAR(20))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Tag VALUES ('a', 'keep'), ('b', 'drop'), ('c', 'drop')",
    );
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "DELETE FROM w USING Widget w JOIN Tag t ON t.wid = w.id WHERE t.label = 'drop'"
        ),
        2
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget"),
        vec![vec!["a"]]
    );

    // A target that is not in the USING relation is an error, not a no-op.
    let err = engine
        .execute(
            &mut session,
            "DELETE FROM Nope USING Widget JOIN Tag ON Tag.wid = Widget.id",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1146);
}

#[test]
fn a_delete_names_its_targets_in_one_place_only() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Tag (wid VARCHAR(21), label VARCHAR(20))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Tag VALUES ('a', 'keep'), ('b', 'drop')",
    );

    // Both spellings at once says two different things: `DELETE Widget FROM
    // Widget, Tag USING ...` names one target before FROM and two after it.
    // MySQL rejects it, and reading either half would delete from a table the
    // other half did not name — which is how this once emptied Tag.
    for sql in [
        "DELETE Widget FROM Widget USING Widget JOIN Tag ON Tag.wid = Widget.id",
        "DELETE Widget FROM Widget, Tag USING Widget JOIN Tag ON Tag.wid = Widget.id",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1064, "{sql}");
    }
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Tag"),
        vec![vec!["2"]]
    );

    // A comma in USING is still a cross join, as it is in a FROM clause.
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "DELETE FROM Widget USING Widget, Tag WHERE Tag.wid = Widget.id"
        ),
        2
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget"),
        vec![vec!["c"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Tag"),
        vec![vec!["2"]]
    );
}

#[test]
fn a_limited_delete_cascades_to_exactly_what_it_removed() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE P (id INT NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE C (
            id INT NOT NULL,
            pid INT NOT NULL,
            PRIMARY KEY (id),
            FOREIGN KEY (pid) REFERENCES P(id) ON DELETE CASCADE
        )",
    );
    run(&engine, &mut session, "INSERT INTO P VALUES (1), (2), (3)");
    run(
        &engine,
        &mut session,
        "INSERT INTO C VALUES (10, 1), (20, 2), (30, 3)",
    );

    // The cascade follows the rows the LIMIT chose, not every row the WHERE
    // matched — the children of the parent that survived have to survive too.
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "DELETE FROM P ORDER BY id DESC LIMIT 2"
        ),
        2
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM P ORDER BY id"),
        vec![vec!["1"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM C ORDER BY id"),
        vec![vec!["10"]]
    );

    // And the whole thing is still one unit of work.
    run(&engine, &mut session, "START TRANSACTION");
    run(&engine, &mut session, "DELETE FROM P ORDER BY id LIMIT 1");
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM P"),
        Vec::<Vec<String>>::new()
    );
    run(&engine, &mut session, "ROLLBACK");
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM P ORDER BY id"),
        vec![vec!["1"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM C ORDER BY id"),
        vec![vec!["10"]]
    );
}

#[test]
fn a_comma_list_is_a_multi_table_delete_too() {
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "CREATE TABLE Tag (wid VARCHAR(21), label VARCHAR(20))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Tag VALUES ('a', 'keep'), ('b', 'drop')",
    );

    // The commoner spelling of a multi-table delete: targets before FROM and a
    // comma list after it, which is a cross join as it is anywhere else. This
    // and the JOIN form take the same path, so refusing one and not the other
    // would be an accident of which guard ran first.
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "DELETE Widget FROM Widget, Tag WHERE Tag.wid = Widget.id"
        ),
        2
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Widget"),
        vec![vec!["c"]]
    );
    // Only the named target lost rows.
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Tag"),
        vec![vec!["2"]]
    );

    // Naming no target at all is refused, because defaulting to every table in
    // the list would empty ones nobody asked to touch.
    let err = engine
        .execute(
            &mut session,
            "DELETE FROM Widget, Tag WHERE Tag.wid = Widget.id",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1064);
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Tag"),
        vec![vec!["2"]]
    );
}

#[test]
fn a_nested_json_constructor_builds_a_document_not_a_string() {
    let (engine, mut session) = engine();

    // The whole reason a JSON value is tagged. Without it the inner call's
    // result reaches the outer one as an ordinary string and is embedded as
    // one: `{"a": "[1,2]"}`, which is valid JSON and the wrong answer.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_OBJECT('a', JSON_ARRAY(1, 2)),
                    JSON_ARRAY(JSON_ARRAY(1), JSON_OBJECT('k', 1)),
                    JSON_OBJECT('a', JSON_OBJECT('b', JSON_ARRAY(1)))"
        ),
        vec![vec![
            r#"{"a": [1, 2]}"#,
            r#"[[1], {"k": 1}]"#,
            r#"{"a": {"b": [1]}}"#
        ]]
    );

    // A string stays a string. Guessing by whether it parses would turn a
    // column holding `[9,9]` into an array nobody asked for.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Doc (id INT, doc JSON, txt VARCHAR(50))",
    );
    run(
        &engine,
        &mut session,
        r#"INSERT INTO Doc VALUES (1, '{"a":[1,2],"b":"s"}', '[9,9]')"#,
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_OBJECT('d', doc), JSON_OBJECT('t', txt) FROM Doc"
        ),
        vec![vec![
            r#"{"d": {"a": [1, 2], "b": "s"}}"#,
            r#"{"t": "[9,9]"}"#
        ]]
    );

    // Every other producer of a document nests too.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_OBJECT('e', JSON_EXTRACT(doc, '$.a')),
                    JSON_OBJECT('c', CAST('[1,2]' AS JSON)),
                    JSON_OBJECT('q', JSON_QUOTE('hi')),
                    JSON_OBJECT('s', JSON_SET(doc, '$.c', 3)),
                    JSON_OBJECT('m', JSON_MERGE_PATCH(doc, JSON_OBJECT('b', 2)))
             FROM Doc"
        ),
        vec![vec![
            r#"{"e": [1, 2]}"#,
            r#"{"c": [1, 2]}"#,
            // JSON_QUOTE produces the *text* of a JSON string, so nesting it
            // quotes it again — which is what MySQL does, because that text is
            // the only thing the function is for.
            r#"{"q": "\"hi\""}"#,
            r#"{"s": {"a": [1, 2], "b": "s", "c": 3}}"#,
            r#"{"m": {"a": [1, 2], "b": 2}}"#
        ]]
    );

    // An aggregate that builds a document nests too, in a query of its own
    // because mixing it with the columns above is an ONLY_FULL_GROUP_BY error.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_OBJECT('n', JSON_ARRAYAGG(id)) FROM Doc"
        ),
        vec![vec![r#"{"n": [1]}"#]]
    );

    // JSON_UNQUOTE and ->> return text by definition, so they nest as strings.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_OBJECT('u', JSON_UNQUOTE(JSON_EXTRACT(doc, '$.b'))),
                    JSON_OBJECT('v', doc->>'$.b')
             FROM Doc"
        ),
        vec![vec![r#"{"u": "s"}"#, r#"{"v": "s"}"#]]
    );
}

#[test]
fn a_json_column_refuses_text_that_is_not_a_document() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE Doc (id INT, doc JSON)");

    // MySQL validates on the way in. Accepting `{"a":1` would make every later
    // read of the column fail instead, a long way from the cause.
    run(&engine, &mut session, r#"INSERT INTO Doc VALUES (0, '{}')"#);
    for sql in [
        r#"INSERT INTO Doc VALUES (1, '{"a":1')"#,
        "INSERT INTO Doc VALUES (1, 'not json')",
        r#"UPDATE Doc SET doc = '['"#,
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 3140, "{sql}");
    }

    // The shapes that are documents are all accepted, including the scalars.
    for literal in [r#"{"a": 1}"#, "[1,2]", "null", "true", "3", r#""s""#] {
        run(
            &engine,
            &mut session,
            &format!("INSERT INTO Doc VALUES (1, '{literal}')"),
        );
    }
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM Doc"),
        vec![vec!["7"]]
    );

    // NULL is not a document and is still allowed.
    run(&engine, &mut session, "INSERT INTO Doc VALUES (2, NULL)");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT doc FROM Doc WHERE id = 2 AND doc IS NULL"
        ),
        vec![vec!["NULL"]]
    );
}

#[test]
fn a_document_read_back_is_still_a_document() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE Doc (id INT, doc JSON)");
    run(
        &engine,
        &mut session,
        r#"INSERT INTO Doc VALUES (1, '{"a":1}')"#,
    );

    // Round-tripping through a table keeps the tag, so what came out of one
    // statement nests correctly in the next.
    run(
        &engine,
        &mut session,
        "INSERT INTO Doc SELECT 2, JSON_OBJECT('w', doc) FROM Doc WHERE id = 1",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_OBJECT('z', doc) FROM Doc WHERE id = 2"
        ),
        vec![vec![r#"{"z": {"w": {"a": 1}}}"#]]
    );

    // And it is still text everywhere text is what was asked for: comparison,
    // concatenation and the wire all see the serialised document.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CONCAT('<', doc, '>'), doc = '{\"a\":1}', LENGTH(doc)
             FROM Doc WHERE id = 1"
        ),
        // The document is stored in its own form, so its length is the
        // length of that. And `doc = '{"a":1}'` is *false* in MySQL whichever
        // way the text is spelled: a JSON object never equals a string
        // operand, because the string is read as a JSON string rather than
        // parsed. #0045.
        vec![vec![r#"<{"a": 1}>"#, "0", "8"]]
    );

    // Including arithmetic. Tagging a value must not change what reading it as
    // a number means, or a JSON column holding `3` stops being 3.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Num (id INT, doc JSON, txt VARCHAR(10))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Num VALUES (1, '3', '3')",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT doc + 0, doc * 2, CAST(doc AS SIGNED), doc = txt FROM Num"
        ),
        vec![vec!["3", "6", "3", "1"]]
    );

    // A column filled from a default holds a document too, so a defaulted row
    // nests the same way an inserted one does. Both routes to one: the
    // declared expression, and the implicit value used to backfill an added
    // NOT NULL column or to fill an omitted one under INSERT IGNORE.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Def (
            id INT,
            doc JSON NOT NULL DEFAULT (JSON_ARRAY()),
            lit JSON NOT NULL DEFAULT ('[1]'),
            -- An expression that yields text rather than a document, so the
            -- coercion is the only thing that can tag it.
            made JSON NOT NULL DEFAULT (CONCAT('[', '2', ']'))
        )",
    );
    run(&engine, &mut session, "INSERT INTO Def (id) VALUES (1)");
    run(
        &engine,
        &mut session,
        "ALTER TABLE Def ADD COLUMN more JSON NOT NULL",
    );
    run(
        &engine,
        &mut session,
        "ALTER TABLE Def ADD COLUMN added JSON DEFAULT ('[7]')",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_OBJECT('d', doc, 'l', lit, 'x', made, 'm', more, 'a', added)
             FROM Def"
        ),
        // Keys come out sorted, which is how the document is serialised.
        vec![vec![
            r#"{"a": [7], "d": [], "l": [1], "m": null, "x": [2]}"#
        ]]
    );

    // A default is checked by the statement that declares it, not by the first
    // read of a row that took it — which is where the failure would otherwise
    // surface, a long way from the CREATE that caused it.
    for sql in [
        "CREATE TABLE Bad (id INT, doc JSON DEFAULT ('{'))",
        "ALTER TABLE Def ADD COLUMN worse JSON DEFAULT ('nope')",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 3140, "{sql}");
    }
}

#[test]
fn a_view_describes_its_columns_exactly_as_the_table_does() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Base (
            id INT NOT NULL,
            small SMALLINT NULL,
            name VARCHAR(40) NOT NULL,
            note TEXT NULL,
            amt DECIMAL(10,2) NULL,
            at DATETIME(3) NULL,
            kind ENUM('x','y') NULL,
            flag BOOLEAN NULL,
            big BIGINT UNSIGNED NULL,
            approx DOUBLE NULL,
            PRIMARY KEY (id)
        )",
    );
    run(
        &engine,
        &mut session,
        "CREATE VIEW Mirror AS
         SELECT id, small, name, note, amt, at, kind, flag, big, approx FROM Base",
    );

    // The whole claim, in one comparison: a view over a table's columns is
    // described the same way the table is. Reporting the wire type instead
    // widened every one of them — `INT` read back as precision 19 and
    // `VARCHAR(40)` as text, so `prisma db pull` generated a schema that did
    // not match the one it was pulled from.
    let describe = |engine: &Engine, session: &mut Session, table: &str| {
        rows(
            engine,
            session,
            &format!(
                "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, IS_NULLABLE,
                        CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION, NUMERIC_SCALE,
                        DATETIME_PRECISION
                 FROM information_schema.columns
                 WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = '{table}'
                 ORDER BY ORDINAL_POSITION"
            ),
        )
    };
    let base = describe(&engine, &mut session, "Base");
    assert_eq!(describe(&engine, &mut session, "Mirror"), base);

    // And it is the real thing, not two matching widenings.
    assert_eq!(
        base[0],
        vec!["id", "int", "int", "NO", "NULL", "10", "0", "NULL"]
    );
    assert_eq!(
        base[2],
        vec![
            "name",
            "varchar",
            "varchar(40)",
            "NO",
            "40",
            "NULL",
            "NULL",
            "NULL"
        ]
    );
    assert_eq!(
        base[5],
        vec![
            "at",
            "datetime",
            "datetime(3)",
            "YES",
            "NULL",
            "NULL",
            "NULL",
            "3"
        ]
    );
    // An approximate type has no fixed number of decimal places, so MySQL
    // answers NULL for its scale rather than 0 — unlike the DECIMAL above,
    // which reports the 2 its declaration gave it.
    assert_eq!(
        base[9],
        vec!["approx", "double", "double", "YES", "NULL", "22", "NULL", "NULL"]
    );

    // Widening a column widens the views over it. A view's shape is worked out
    // once and stored, so without re-resolving it every view keeps describing
    // the old width — invisible while everything was widened to text, and a
    // silently wrong schema now that the width is exact. MySQL re-resolves a
    // view when it is opened.
    run(
        &engine,
        &mut session,
        "ALTER TABLE Base MODIFY name VARCHAR(80) NOT NULL",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_TYPE FROM information_schema.columns
             WHERE TABLE_NAME = 'Mirror' AND COLUMN_NAME = 'name'"
        ),
        vec![vec!["varchar(80)"]]
    );
    assert_eq!(
        describe(&engine, &mut session, "Mirror"),
        describe(&engine, &mut session, "Base")
    );
    run(
        &engine,
        &mut session,
        "ALTER TABLE Base MODIFY name VARCHAR(40) NOT NULL",
    );

    // `CREATE VIEW v (a, b)` renames the columns and keeps their types.
    run(
        &engine,
        &mut session,
        "CREATE VIEW Named (a, b) AS SELECT id, name FROM Base",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, COLUMN_TYPE FROM information_schema.columns
             WHERE TABLE_NAME = 'Named' ORDER BY ORDINAL_POSITION"
        ),
        vec![vec!["a", "int"], vec!["b", "varchar(40)"]]
    );

    // Grouping a column does not compute it, so it keeps its type: otherwise
    // `SELECT id FROM t GROUP BY id` describes itself differently from
    // `SELECT id FROM t`, and two views over the same column disagree.
    run(
        &engine,
        &mut session,
        "CREATE VIEW Grouped AS SELECT id, name FROM Base GROUP BY id, name",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, COLUMN_TYPE, NUMERIC_PRECISION, CHARACTER_MAXIMUM_LENGTH
             FROM information_schema.columns
             WHERE TABLE_NAME = 'Grouped' ORDER BY ORDINAL_POSITION"
        ),
        vec![
            vec!["id", "int", "10", "NULL"],
            vec!["name", "varchar(40)", "NULL", "40"]
        ]
    );

    // A computed column has no declared type to keep, so it stays widened —
    // honest rather than invented, and the same widening it always had. The
    // spelling and the reported length agree, which they did not while the
    // fallback carried no width: `varchar(512)` beside a maximum of 65535.
    run(
        &engine,
        &mut session,
        "CREATE VIEW Computed AS SELECT id, UPPER(name) AS up FROM Base",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, DATA_TYPE FROM information_schema.columns
             WHERE TABLE_NAME = 'Computed' ORDER BY ORDINAL_POSITION"
        ),
        vec![vec!["id", "int"], vec!["up", "varchar"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_TYPE, CHARACTER_MAXIMUM_LENGTH FROM information_schema.columns
             WHERE TABLE_NAME = 'Computed' AND COLUMN_NAME = 'up'"
        ),
        vec![vec!["varchar(512)", "512"]]
    );

    // The type travels through every shape a view is written in, because each
    // of them carries the column rather than rebuilding it: a view of a view,
    // a wildcard, a join, a union, and a derived table.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Side (bid INT NULL, label VARCHAR(12) NULL)",
    );
    for (name, definition) in [
        ("Nested", "SELECT * FROM Mirror"),
        ("Star", "SELECT * FROM Base"),
        (
            "Joined",
            "SELECT Base.id, Side.label FROM Base JOIN Side ON Side.bid = Base.id",
        ),
        (
            "United",
            "SELECT id, name FROM Base UNION SELECT bid, label FROM Side",
        ),
        (
            "Derived",
            "SELECT d.id, d.name FROM (SELECT id, name FROM Base) d",
        ),
    ] {
        run(
            &engine,
            &mut session,
            &format!("CREATE VIEW {name} AS {definition}"),
        );
    }
    let types = |engine: &Engine, session: &mut Session, view: &str| {
        rows(
            engine,
            session,
            &format!(
                "SELECT COLUMN_NAME, COLUMN_TYPE FROM information_schema.columns
                 WHERE TABLE_NAME = '{view}' ORDER BY ORDINAL_POSITION"
            ),
        )
    };
    assert_eq!(
        types(&engine, &mut session, "Nested"),
        types(&engine, &mut session, "Mirror")
    );
    assert_eq!(
        types(&engine, &mut session, "Star"),
        types(&engine, &mut session, "Base")
    );
    assert_eq!(
        types(&engine, &mut session, "Joined"),
        vec![vec!["id", "int"], vec!["label", "varchar(12)"]]
    );
    // A union takes the wider of the two declarations, whichever side it is
    // on: `Side.bid` is an `INT` like `Base.id`, and `label` is a
    // `VARCHAR(12)` against `name`'s `VARCHAR(40)`.
    assert_eq!(
        types(&engine, &mut session, "United"),
        vec![vec!["id", "int"], vec!["name", "varchar(40)"]]
    );

    // Which matters most where the two branches are different widths of the
    // same kind: reporting the left one describes a column narrower than what
    // it returns, and a tool generating a schema from that truncates.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Wide (big BIGINT NULL, small SMALLINT NULL)",
    );
    run(
        &engine,
        &mut session,
        "CREATE VIEW Mixed AS SELECT small FROM Wide UNION SELECT big FROM Wide",
    );
    run(
        &engine,
        &mut session,
        "CREATE VIEW Same AS SELECT small FROM Wide UNION SELECT small FROM Wide",
    );
    // Widened to what holds both, which is MySQL's rule: describing the
    // column as the left branch's SMALLINT would have the binary protocol
    // write two bytes for a value that needs eight.
    assert_eq!(
        types(&engine, &mut session, "Mixed"),
        vec![vec!["small", "bigint"]]
    );
    assert_eq!(
        types(&engine, &mut session, "Same"),
        vec![vec!["small", "smallint"]]
    );
    assert_eq!(
        types(&engine, &mut session, "Derived"),
        vec![vec!["id", "int"], vec!["name", "varchar(40)"]]
    );
}

#[test]
fn a_null_argument_makes_the_answer_null() {
    let (engine, mut session) = engine();

    // MySQL returns NULL from a scalar function if any argument is NULL. This
    // engine read the NULL as an empty string in most of them, so `UPPER(NULL)`
    // was `''` — and `WHERE UPPER(t) = ''` then matched rows MySQL does not
    // return, which is a filter quietly keeping the wrong set.
    let nulls = vec![vec!["NULL"; 5]];
    for sql in [
        "SELECT UPPER(NULL), LOWER(NULL), UCASE(NULL), LCASE(NULL), REVERSE(NULL)",
        "SELECT TRIM(NULL), LTRIM(NULL), RTRIM(NULL), SPACE(NULL), MD5(NULL)",
        "SELECT LENGTH(NULL), CHAR_LENGTH(NULL), OCTET_LENGTH(NULL), INSTR(NULL, 'a'), \
         LOCATE('a', NULL)",
        "SELECT LEFT(NULL, 2), RIGHT(NULL, 2), SUBSTRING(NULL, 1, 2), MID(NULL, 1, 2), \
         REPLACE(NULL, 'a', 'b')",
        "SELECT LPAD(NULL, 3, 'x'), RPAD(NULL, 3, 'x'), CONCAT(NULL, 'a'), \
         DATE(NULL), DATE_FORMAT(NULL, '%Y')",
        "SELECT YEAR(NULL), MONTH(NULL), DAY(NULL), HOUR(NULL), QUARTER(NULL)",
        "SELECT ABS(NULL), ROUND(NULL, 1), MOD(NULL, 2), GREATEST(NULL, 1), LEAST(NULL, 1)",
        "SELECT JSON_VALID(NULL), JSON_TYPE(NULL), JSON_QUOTE(NULL), JSON_LENGTH(NULL), \
         DATEDIFF(NULL, '2026-01-01')",
    ] {
        assert_eq!(rows(&engine, &mut session, sql), nulls, "{sql}");
    }

    // The rule keys on having a NULL argument rather than on the name, so a
    // function called with none is untouched: `UNIX_TIMESTAMP()` still means
    // now, where `UNIX_TIMESTAMP(NULL)` used to answer with the current time.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT UNIX_TIMESTAMP(NULL), UNIX_TIMESTAMP() > 0"
        ),
        vec![vec!["NULL", "1"]]
    );

    // And the exceptions, each of which means something with a NULL: a test
    // for one, a function documented to skip them, and the JSON constructors,
    // which store a JSON null rather than becoming one.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COALESCE(NULL, 1), IFNULL(NULL, 1), IF(NULL, 1, 2), ISNULL(NULL),
                    NULLIF(NULL, 1), NULLIF(1, NULL), CONCAT_WS('-', NULL, 'a')"
        ),
        vec![vec!["1", "1", "2", "1", "NULL", "1", "a"]]
    );
    // CONCAT_WS skips NULL *values*; a NULL separator is still NULL. Exempting
    // the function from the rule wholesale answered 'ab' here.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CONCAT_WS(NULL, 'a', 'b'), IF(1, NULL, 2), COALESCE(NULL, NULL)"
        ),
        vec![vec!["NULL", "NULL", "NULL"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(NULL), JSON_OBJECT('k', NULL),
                    JSON_SET('{}', '$.k', NULL), JSON_ARRAY_APPEND('[1]', '$', NULL)"
        ),
        vec![vec![
            "[null]",
            r#"{"k": null}"#,
            r#"{"k": null}"#,
            "[1, null]"
        ]]
    );
    // A NULL *path* is not a NULL value: it makes the call NULL, where the
    // value above is stored as a JSON null. Exempting the function wholesale
    // sent the NULL to the path parser and failed with 3143.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT JSON_SET('{"a":1}', NULL, 2), JSON_ARRAY_APPEND('[1]', NULL, 2)"#
        ),
        vec![vec!["NULL", "NULL"]]
    );

    // A function this engine does not implement still says so. Short-circuiting
    // on the NULL before recognising the name let `SELECT NOT_A_FUNCTION(NULL)`
    // answer, and a query over an all-NULL column would then succeed until the
    // day one row had a value in it.
    for sql in [
        "SELECT NOT_A_FUNCTION(NULL)",
        "SELECT SOUNDEX(NULL)",
        "SELECT FROM_UNIXTIME(NULL)",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1235, "{sql}");
    }

    // The shape that motivated it: a filter over a function of a NULL column
    // must not match, and the row must not vanish from an unfiltered read.
    run(
        &engine,
        &mut session,
        "CREATE TABLE N (id INT, t VARCHAR(20))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO N VALUES (1, 'Hello'), (2, NULL)",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM N WHERE UPPER(t) = ''"
        ),
        Vec::<Vec<String>>::new()
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, UPPER(t), LENGTH(t) FROM N ORDER BY id"
        ),
        vec![vec!["1", "HELLO", "5"], vec!["2", "NULL", "NULL"]]
    );
}

#[test]
fn a_document_is_written_the_way_mysql_writes_one() {
    let (engine, mut session) = engine();

    // A space after every comma and every colon, and an object's keys ordered
    // by byte length first and then bytewise — which is not the lexicographic
    // order a Rust map iterates in. `{"a": 2, "z": 1, "yy": 3}`, not
    // `{"a":2,"yy":3,"z":1}`: a different order, not only a different spacing.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_OBJECT('z', 1, 'a', 2, 'yy', 3, 'A', 4)"
        ),
        vec![vec![r#"{"A": 4, "a": 2, "z": 1, "yy": 3}"#]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT CAST('{"b":1,"a":2,"aa":3,"A":4}' AS JSON)"#
        ),
        vec![vec![r#"{"A": 4, "a": 2, "b": 1, "aa": 3}"#]]
    );

    // Empty containers, nesting, and the scalars serde already renders the way
    // MySQL does — checked against it rather than assumed.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT CAST('{}' AS JSON), CAST('[]' AS JSON), CAST('[[],{}]' AS JSON),
                      CAST('[1.5, 2.0, 1e10, -0.0]' AS JSON),
                      CAST('[true,false,null]' AS JSON)"#
        ),
        vec![vec![
            "{}",
            "[]",
            "[[], {}]",
            "[1.5, 2.0, 10000000000.0, -0.0]",
            "[true, false, null]"
        ]]
    );

    // JSON_UNQUOTE of a document is still that document's text, so it is
    // written the same way. Rendering it compactly here put two spellings of
    // the same shape in one row: `SELECT doc, doc->>'$.a'`.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT JSON_UNQUOTE(CAST('{"a":1}' AS JSON)),
                      JSON_UNQUOTE(CAST('[1,2]' AS JSON)),
                      JSON_UNQUOTE(CAST('"s"' AS JSON))"#
        ),
        vec![vec![r#"{"a": 1}"#, "[1, 2]", "s"]]
    );

    // A JSON column holds the document rather than the text it was written as,
    // so the spacing and the key order come back MySQL's way whatever was
    // typed. Keeping the input verbatim made every column a record of how it
    // happened to be spelled.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Norm (id INT, kept JSON)",
    );
    run(
        &engine,
        &mut session,
        r#"INSERT INTO Norm VALUES (1, '{"b":1,  "a":2}'), (2, '[ 1 , 2 ]')"#,
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT kept FROM Norm ORDER BY id"),
        vec![vec![r#"{"a": 2, "b": 1}"#], vec!["[1, 2]"]]
    );
}

#[test]
fn a_column_is_described_with_the_type_code_its_width_earns() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE W (
            t TINYINT, s SMALLINT, m MEDIUMINT, i INT, b BIGINT,
            c CHAR(5), v VARCHAR(40), x TEXT, bl BLOB, vb VARBINARY(10),
            ts TIMESTAMP NULL, dt DATETIME, e ENUM('a','b')
        )",
    );
    // MySQL's codes are finer than this engine's kinds, and describing a whole
    // kind with one code told a typed client the wrong width for nearly every
    // column. Each of these was checked against MySQL 8.3 rather than read off
    // a table.
    let described = engine.describe(&session, "SELECT * FROM W", 0).unwrap();
    let codes: Vec<String> = described.iter().map(|c| format!("{:?}", c.ty)).collect();
    assert_eq!(
        codes,
        vec![
            "Tiny",
            "Short",
            "Int24",
            "Long",
            "LongLong",
            "String",
            "VarString",
            "Blob",
            "Blob",
            "VarString",
            "Timestamp",
            "DateTime",
            "String",
        ]
    );

    // A `TEXT` and a `BLOB` share a code, and only the charset says which is
    // which. Getting that wrong sent a text column as bytes, which Prisma
    // base64s and then refuses to read as a String.
    let textual: Vec<bool> = described
        .iter()
        .map(|c| c.to_wire().charset != 63)
        .collect();
    assert_eq!(
        textual,
        vec![
            false, false, false, false, false, // the integers are binary
            true, true, true, // char, varchar, text carry text
            false, false, // blob and varbinary carry bytes
            false, false, // the temporals are binary
            true,  // an enum carries text
        ]
    );
}

#[test]
fn a_column_refuses_a_value_its_width_cannot_hold() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE R (t TINYINT, tu TINYINT UNSIGNED, s SMALLINT, m MEDIUMINT, i INT)",
    );

    // Every boundary is accepted on both sides.
    run(
        &engine,
        &mut session,
        "INSERT INTO R VALUES (127, 255, 32767, 8388607, 2147483647)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO R VALUES (-128, 0, -32768, -8388608, -2147483648)",
    );

    // And a step past any of them is refused. Not a formality: the binary
    // protocol encodes by the declared type, so a TINYINT allowed to hold 999
    // would reach a client as 231. Describing a column honestly is only safe
    // once it cannot hold something wider.
    for (sql, what) in [
        ("INSERT INTO R (t) VALUES (128)", "tinyint high"),
        ("INSERT INTO R (t) VALUES (-129)", "tinyint low"),
        ("INSERT INTO R (tu) VALUES (256)", "unsigned tinyint high"),
        ("INSERT INTO R (tu) VALUES (-1)", "unsigned tinyint low"),
        ("INSERT INTO R (s) VALUES (32768)", "smallint"),
        ("INSERT INTO R (m) VALUES (8388608)", "mediumint"),
        ("INSERT INTO R (i) VALUES (2147483648)", "int"),
        ("UPDATE R SET t = 999", "update"),
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1264, "{what}: {sql}");
    }

    // Nothing that was refused was written, and nothing that was accepted was
    // lost.
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(*) FROM R"),
        vec![vec!["2"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT t FROM R ORDER BY t"),
        vec![vec!["-128"], vec!["127"]]
    );
}

#[test]
fn a_column_that_cannot_hold_a_value_is_not_described_as_if_it_could() {
    let (engine, mut session) = engine();

    // The invariant the narrow type codes rest on. The binary protocol encodes
    // by the declared code, so anything that can produce a value wider than
    // the code it is announced under sends the wrong bytes — the client reads
    // 300 as 44 and never knows.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Q (t TINYINT, iu INT UNSIGNED, y YEAR, id TINYINT AUTO_INCREMENT PRIMARY KEY)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Q (t) VALUES (100), (100), (100)",
    );

    // A total outgrows what it totals, so SUM and AVG are decimals rather than
    // the argument's width. MIN and MAX stay, because they are values of it.
    let described = engine
        .describe(
            &session,
            "SELECT SUM(t), AVG(t), MIN(t), MAX(t), COUNT(t) FROM Q",
            0,
        )
        .unwrap();
    let codes: Vec<String> = described.iter().map(|c| format!("{:?}", c.ty)).collect();
    assert_eq!(
        codes,
        vec!["NewDecimal", "NewDecimal", "Tiny", "Tiny", "LongLong"]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT SUM(t) FROM Q"),
        vec![vec!["300"]]
    );

    // An UNSIGNED column has to say so, or the binary protocol reads its bytes
    // back signed and three billion arrives negative.
    let described = engine.describe(&session, "SELECT iu FROM Q", 0).unwrap();
    assert!(described[0].to_wire().flags & 0x0020 != 0, "UNSIGNED_FLAG");

    // A YEAR is two bytes, and was the one integer still able to outgrow its
    // code: 99999 reached a binary client as 34463.
    for sql in [
        "INSERT INTO Q (y) VALUES (99999)",
        "INSERT INTO Q (y) VALUES (1900)",
        "INSERT INTO Q (y) VALUES (2156)",
    ] {
        assert_eq!(
            engine.execute(&mut session, sql, &[]).unwrap_err().code,
            1264,
            "{sql}"
        );
    }
    run(
        &engine,
        &mut session,
        "INSERT INTO Q (y) VALUES (1901), (2155), (0)",
    );

    // And a generated id is subject to the width like any other value: a
    // TINYINT AUTO_INCREMENT counting past 127 was stored and sent as one
    // byte, arriving as -128.
    run(&engine, &mut session, "DELETE FROM Q");
    run(
        &engine,
        &mut session,
        "INSERT INTO Q (id, t) VALUES (127, 1)",
    );
    assert_eq!(
        engine
            .execute(&mut session, "INSERT INTO Q (t) VALUES (2)", &[])
            .unwrap_err()
            .code,
        1264
    );
}

#[test]
fn a_union_is_described_as_what_holds_both_branches() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE U (
            i INT, iu INT UNSIGNED, s SMALLINT, b BIGINT,
            d DECIMAL(10,2), db DOUBLE, dt DATE, dtm DATETIME,
            t TEXT, lt LONGTEXT
        )",
    );

    // Each pair checked against MySQL 8.3 rather than reasoned about. The
    // signed/unsigned pair is the one that looks like it needs no widening:
    // both are `LONG`, and neither range holds the other's.
    let code = |engine: &Engine, session: &mut Session, sql: &str| {
        format!("{:?}", engine.describe(session, sql, 0).unwrap()[0].ty)
    };
    for (sql, expected) in [
        ("SELECT s FROM U UNION SELECT b FROM U", "LongLong"),
        ("SELECT i FROM U UNION SELECT iu FROM U", "LongLong"),
        ("SELECT i FROM U UNION SELECT d FROM U", "NewDecimal"),
        ("SELECT db FROM U UNION SELECT i FROM U", "Double"),
        ("SELECT dt FROM U UNION SELECT dtm FROM U", "DateTime"),
        ("SELECT i FROM U UNION SELECT t FROM U", "VarString"),
    ] {
        assert_eq!(code(&engine, &mut session, sql), expected, "{sql}");
    }

    // A `TEXT` and a `LONGTEXT` share a code, so the union keeps a declared
    // type — which is the only thing saying the blob carries text. Losing it
    // here announced the column as binary and Prisma base64'd it.
    let described = engine
        .describe(&session, "SELECT t FROM U UNION SELECT lt FROM U", 0)
        .unwrap();
    assert_eq!(format!("{:?}", described[0].ty), "Blob");
    assert_ne!(described[0].to_wire().charset, 63, "should carry text");
}

#[test]
fn a_not_null_column_says_so_whatever_its_type() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE N (
            id INT NOT NULL, nn VARCHAR(9) NOT NULL, ny VARCHAR(9) NULL,
            inn INT NOT NULL, tn TEXT NOT NULL, dn DATETIME NOT NULL,
            en ENUM('a') NOT NULL, jn JSON NOT NULL, bn BLOB NOT NULL,
            PRIMARY KEY (id)
        )",
    );
    run(&engine, &mut session, "CREATE TABLE M (aid INT NULL)");

    // Read off the wire definition, not the engine's own `nullable` field.
    // The bug was in `to_wire`, so a test asking the field it is derived from
    // passes whether or not the flag ever reaches the client — which this one
    // did, until breaking the fix failed to fail it.
    let nullable = |engine: &Engine, session: &Session, sql: &str| -> Vec<bool> {
        engine
            .describe(session, sql, 0)
            .unwrap()
            .iter()
            .map(|c| c.to_wire().flags & mysql_ultralight_protocol::column::flags::NOT_NULL == 0)
            .collect()
    };

    // The flag was only ever *cleared*, and `ColumnDef::new` only turned it on
    // for the numeric codes — so a nullable INT was right by being switched
    // off again, while a NOT NULL VARCHAR never had it switched on at all.
    // Generated code then modelled a non-null column as optional.
    assert_eq!(
        nullable(&engine, &session, "SELECT * FROM N"),
        vec![false, false, true, false, false, false, false, false, false]
    );

    // An outer join can invent a row of NULLs for the side it does not
    // preserve, so a NOT NULL column there stops being one.
    assert_eq!(
        nullable(
            &engine,
            &session,
            "SELECT nn, inn FROM N LEFT JOIN M ON M.aid = N.id"
        ),
        vec![false, false]
    );
    assert_eq!(
        nullable(
            &engine,
            &session,
            "SELECT M.aid, N.nn FROM M LEFT JOIN N ON N.id = M.aid"
        ),
        vec![true, true]
    );

    // And a union is nullable if either branch is — which only became visible
    // once NOT NULL was reported at all.
    assert_eq!(
        nullable(&engine, &session, "SELECT nn FROM N UNION SELECT ny FROM N"),
        vec![true]
    );
    assert_eq!(
        nullable(&engine, &session, "SELECT nn FROM N UNION SELECT nn FROM N"),
        vec![false]
    );

    // A RIGHT JOIN pads the *left* side. Relaxing the right one for both
    // operators had the engine flag a column NOT NULL and then send NULL in
    // it, which is a lie a client cannot recover from.
    assert_eq!(
        nullable(
            &engine,
            &session,
            "SELECT id, nn FROM N RIGHT JOIN M ON M.aid = N.id"
        ),
        vec![true, true]
    );
    assert_eq!(
        nullable(
            &engine,
            &session,
            "SELECT id, nn FROM N LEFT JOIN M ON M.aid = N.id"
        ),
        vec![false, false]
    );
    // One matching row and one that does not, so the join actually pads.
    run(
        &engine,
        &mut session,
        "INSERT INTO N VALUES (1,'a','b',2,'t','2026-01-01 00:00:00','a','{}','x')",
    );
    run(&engine, &mut session, "INSERT INTO M VALUES (1), (99)");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM N RIGHT JOIN M ON M.aid = N.id ORDER BY id"
        ),
        vec![vec!["NULL"], vec!["1"]]
    );

    // And a literal branch is not nullable. Treating every computed column as
    // if it were imported a nullability that does not exist, so a union with a
    // constant reported a NOT NULL column as optional.
    for (sql, expected) in [
        ("SELECT id FROM N UNION SELECT 1", false),
        ("SELECT id FROM N UNION SELECT 1 + 1", false),
        ("SELECT id FROM N UNION ALL SELECT 2", false),
        ("SELECT id FROM N UNION SELECT NULL", true),
    ] {
        assert_eq!(nullable(&engine, &session, sql), vec![expected], "{sql}");
    }
}

#[test]
fn a_computed_column_is_described_by_its_expression() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE E (i INT, d DECIMAL(10,2), f DOUBLE, s VARCHAR(20), dt DATETIME, j JSON)",
    );
    let codes = |engine: &Engine, session: &Session, sql: &str| -> Vec<String> {
        engine
            .describe(session, sql, 0)
            .unwrap()
            .iter()
            .map(|c| format!("{:?}", c.ty))
            .collect()
    };

    // Every one of these was `VarString` — a third of everything the
    // differential harness had to say. All measured against MySQL 8.3, because
    // the asymmetries are not guessable: `LENGTH` is a number while `CONCAT`
    // is a string, `JSON_LENGTH` a number while `JSON_TYPE` is a string.
    for (sql, expected) in [
        (
            "SELECT LENGTH(s), CHAR_LENGTH(s), INSTR(s,'a'), LOCATE('a',s) FROM E",
            vec!["LongLong"; 4],
        ),
        (
            "SELECT CONCAT(s,'x'), UPPER(s), TRIM(s), SUBSTRING(s,1,2) FROM E",
            vec!["VarString"; 4],
        ),
        (
            "SELECT s LIKE 'a%', s IS NULL, i = 1, i > 1, NOT i FROM E",
            vec!["LongLong"; 5],
        ),
        (
            "SELECT i IN (1,2), i BETWEEN 1 AND 2, i IS NOT NULL FROM E",
            vec!["LongLong"; 3],
        ),
        // A cast says what it wants — and a cast to CHAR makes a *varying*
        // string where a declared CHAR column is a fixed one.
        (
            "SELECT CAST(i AS CHAR), CAST(s AS UNSIGNED), CAST(i AS DECIMAL(8,2)) FROM E",
            vec!["VarString", "LongLong", "NewDecimal"],
        ),
        // These hand back an argument, widened to LONGLONG for integers —
        // except NULLIF, which keeps the argument's own width.
        (
            "SELECT COALESCE(i,0), IFNULL(i,0), IF(1,i,0), ISNULL(i), NULLIF(i,5) FROM E",
            vec!["LongLong", "LongLong", "LongLong", "LongLong", "Long"],
        ),
        // Arithmetic widens; division is exact even over two integers.
        (
            "SELECT i+1, i*2, i/2, i%2, -i, i+d, i+f FROM E",
            vec![
                "LongLong",
                "LongLong",
                "NewDecimal",
                "LongLong",
                "LongLong",
                "NewDecimal",
                "Double",
            ],
        ),
        // A literal with a point is a DECIMAL to MySQL, and NULL has a code.
        (
            "SELECT 1, 'x', 1.5, NULL, TRUE",
            vec!["LongLong", "VarString", "NewDecimal", "Null", "LongLong"],
        ),
        (
            "SELECT JSON_OBJECT('a',1), JSON_TYPE(j), JSON_LENGTH(j), j->'$', j->>'$' FROM E",
            vec!["Json", "VarString", "LongLong", "Json", "LongBlob"],
        ),
        (
            "SELECT YEAR(dt), MONTH(dt), DATEDIFF(dt,dt), NOW(), CURDATE() FROM E",
            vec!["Year", "LongLong", "LongLong", "DateTime", "Date"],
        ),
        // DATE_ADD gives back what it was given, which is usually a string.
        (
            "SELECT DATE_ADD(dt, INTERVAL 1 DAY), DATE_ADD('2026-01-01', INTERVAL 1 DAY),
                    TIMESTAMPADD(HOUR, 1, dt) FROM E",
            // A string argument gives MySQL's fixed-width `STRING`, and
            // TIMESTAMPADD puts its unit first so the value is the third.
            vec!["DateTime", "String", "DateTime"],
        ),
        (
            "SELECT CASE WHEN i=1 THEN 'a' ELSE 'b' END, CASE WHEN i=1 THEN 1 ELSE 2 END FROM E",
            vec!["VarString", "LongLong"],
        ),
        // Typed by their arguments rather than fixed: ABS and ROUND of a
        // decimal are decimals, and GREATEST hands back whichever argument
        // won. Calling these LONGLONG had the binary protocol send 0 for a
        // string and drop a decimal's fraction.
        (
            "SELECT ABS(d), ROUND(d,1), GREATEST(s,'z'), LEAST(s,'z') FROM E",
            vec!["NewDecimal", "NewDecimal", "VarString", "VarString"],
        ),
        // Arithmetic over something that is not known to be a number is a
        // double, because that is what the evaluator produces: `'3.5' + 1` is
        // 4.5, and calling it an integer sent 4.
        ("SELECT s + 1 FROM E", vec!["Double"]),
        // An alternative is typed by *all* of its branches, not the first: a
        // TINYINT beside 1000 is one byte wide if only the first is read, and
        // the client gets 232.
        (
            "SELECT CASE WHEN 0 THEN i ELSE 'txt' END, COALESCE(i,'x'), IF(0,i,'s') FROM E",
            vec!["VarString"; 3],
        ),
        // An exponent literal is a double where one with a point is a decimal.
        ("SELECT 1e3, 1.5", vec!["Double", "NewDecimal"]),
        // POSITION and EXTRACT have their own grammar in the parser, so they
        // never reach the by-name table and answered VAR_STRING. Their
        // synonyms LOCATE and INSTR do arrive as calls, and always agreed.
        (
            "SELECT POSITION('b' IN s), LOCATE('b', s), INSTR(s, 'b'), EXTRACT(YEAR FROM dt) FROM E",
            vec!["LongLong"; 4],
        ),
        // The same grammar split, the other way round: these are strings, and
        // were only right because the fallback happens to be a string too.
        (
            "SELECT SUBSTRING(s, 2), SUBSTR(s, 2), MID(s, 2, 1), TRIM(s), LTRIM(s) FROM E",
            vec!["VarString"; 5],
        ),
    ] {
        assert_eq!(codes(&engine, &session, sql), expected, "{sql}");
    }

    // `SELECT IF()` reaches the planner before anything rejects the call, and
    // indexing its empty argument list aborted the process — `panic = "abort"`
    // means any client could stop the server with one token.
    assert!(engine.execute(&mut session, "SELECT IF()", &[]).is_ok());
    assert_eq!(
        rows(&engine, &mut session, "SELECT 'still here'"),
        vec![vec!["still here"]]
    );
}
