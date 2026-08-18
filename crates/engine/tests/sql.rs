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

/// MySQL accepts an IN list whose elements are scalar subqueries. It is
/// rewritten into a UNION ALL, so this checks the result, not just the parse.
#[test]
fn in_a_list_of_scalar_subqueries() {
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
fn dml_clauses_that_are_not_implemented_are_refused() {
    // Each of these parses and used to be dropped on the floor, which turns a
    // one-row statement into a whole-table one.
    let (engine, mut session) = engine();
    widgets(&engine, &mut session);

    for sql in [
        "UPDATE Widget SET count = 9 LIMIT 1",
        "UPDATE Widget SET count = 9 ORDER BY id LIMIT 1",
        "DELETE FROM Widget LIMIT 1",
        "DELETE FROM Widget ORDER BY id LIMIT 1",
        "DELETE FROM Widget USING Widget WHERE id = 'a'",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1235, "expected ER_NOT_SUPPORTED_YET for: {sql}");
    }

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
