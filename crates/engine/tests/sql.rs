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

/// The differential harness asks this exact statement to prove the server it
/// reached is the one its caller started, and fails the run when the answer is
/// not the id that server printed. Answering it from anywhere but the running
/// server's own configuration would make that check pass against a leftover
/// process, which is the false green the check exists to stop.
#[test]
fn the_instance_id_is_readable_over_sql() {
    let config = ServerConfig {
        instance: "0123456789abcdef".to_owned(),
        ..Default::default()
    };
    let engine = Engine::new(config.clone());
    engine.bootstrap_database("testdb").unwrap();
    let mut session = Session::new(config, 1);
    session.current_database = Some("testdb".to_owned());

    assert_eq!(
        rows(&engine, &mut session, "SELECT @@ultralight_instance"),
        vec![vec!["0123456789abcdef".to_owned()]]
    );
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

/// The ESCAPE clause names a character, and the character has to come out of
/// the literal's *value*.
///
/// `Display` on a `Value` prints the SQL that would produce it, so reading the
/// first character of the rendering gave back the opening quote and every
/// ESCAPE clause in this engine escaped with `'` whatever was written (#0093).
///
/// The second column is the one that decides it: with `!` escaping, the
/// pattern is `a` then a literal quote then a wildcard, which `a%` does not
/// match. Only an engine escaping with `'` can answer 1. Measured on 8.3.
#[test]
fn like_takes_its_escape_from_the_literals_value_and_not_its_rendering() {
    let (engine, mut session) = engine();

    let answers = rows(
        &engine,
        &mut session,
        "SELECT 'a%' LIKE 'a!%' ESCAPE '!', 'a%' LIKE 'a''%' ESCAPE '!', \
         'a%' NOT LIKE 'a!%' ESCAPE '!', 'a%' NOT LIKE 'a''%' ESCAPE '!'",
    );

    assert_eq!(answers, vec![vec!["1", "0", "0", "1"]]);
}

/// An empty ESCAPE turns escaping off; a NULL one does not.
///
/// The two look interchangeable and are not: `ESCAPE ''` leaves the pattern
/// with no escape character at all, while `ESCAPE NULL` leaves the default
/// backslash in place — the one place a NULL operand does not propagate.
/// Both columns here hold the same pattern, `a\%`. Measured on 8.3.
#[test]
fn an_empty_escape_disables_escaping_where_a_null_escape_keeps_the_default() {
    let (engine, mut session) = engine();

    let answers = rows(
        &engine,
        &mut session,
        r"SELECT 'a%' LIKE 'a\%' ESCAPE '', 'a%' LIKE 'a\%' ESCAPE NULL",
    );

    assert_eq!(answers, vec![vec!["0", "1"]]);
}

/// A trailing escape character has nothing to escape, and matches itself.
///
/// Not a failed pattern, which is what this engine answered: 8.3 reads the
/// last character literally, so `'a!' LIKE 'a!' ESCAPE '!'` is 1 — and
/// `'a%' LIKE 'a%' ESCAPE '%'` is too, because the wildcard reading is off
/// even in the position where the escaping cannot apply.
#[test]
fn a_trailing_escape_character_matches_itself() {
    let (engine, mut session) = engine();

    let answers = rows(
        &engine,
        &mut session,
        "SELECT 'a!' LIKE 'a!' ESCAPE '!', 'a' LIKE 'a!' ESCAPE '!', \
         'a%' LIKE 'a%' ESCAPE '%', 'a!!' LIKE 'a!!!' ESCAPE '!'",
    );

    assert_eq!(answers, vec![vec!["1", "0", "1", "1"]]);
}

/// ESCAPE takes one character, counted in characters rather than bytes.
///
/// A non-string is converted to one first, which is how a *number* reaches
/// this error: `1.5` is three characters. `é` is two bytes and one character,
/// so it is accepted. All measured on 8.3 over a utf8mb4 connection.
#[test]
fn an_escape_of_more_than_one_character_is_rejected() {
    let (engine, mut session) = engine();

    for sql in [
        "SELECT 'a%' LIKE 'a!%' ESCAPE '!!'",
        "SELECT 'a%' LIKE 'a!%' ESCAPE 'ab'",
        "SELECT 'x%' LIKE 'x1%' ESCAPE 1.5",
        // Resolved before the operands are read, so the NULL does not get to
        // answer NULL first: 8.3 raises this while resolving the item.
        "SELECT NULL LIKE 'a' ESCAPE '!!'",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1210, "expected ER_WRONG_ARGUMENTS for: {sql}");
        assert_eq!(err.sql_state, "HY000", "for: {sql}");
    }

    let accepted = rows(
        &engine,
        &mut session,
        "SELECT 'a%' LIKE CONCAT('a', 'é', '%') ESCAPE 'é', \
         'x%' LIKE 'x1%' ESCAPE 1, 'x%' LIKE 'x!%' ESCAPE x'21'",
    );
    assert_eq!(accepted, vec![vec!["1", "1", "1"]]);
}

/// The error a statement was refused with, naming it if it was not refused.
///
/// `unwrap_err` panics with the result set instead, which in a loop over a
/// dozen near-identical statements says nothing about which one got through —
/// and telling them apart is the entire content of the test below.
fn refusal_of(
    engine: &Engine,
    session: &mut Session,
    sql: &str,
) -> mysql_ultralight_engine::EngineError {
    match engine.execute(session, sql, &[]) {
        Err(e) => e,
        Ok(_) => panic!("expected a refusal, got an answer for: {sql}"),
    }
}

/// A malformed ESCAPE is settled while the item is resolved, not per row.
///
/// The distinction is the whole test. Evaluating the clause where the pattern
/// is matched means it is never settled for a statement that matches nothing:
/// over an empty table, and down a CASE branch nothing takes. 8.3 raises 1210
/// for both, and this engine answered an empty result set and `9`.
///
/// Every statement here is chosen so the LIKE is evaluated for *no* row, which
/// is what a mutation moving the check back into the row path cannot survive.
/// A bare-aggregate HAVING is not such a statement and was here as one: it runs
/// over a single implicit group whatever the table holds, so it was answered by
/// the row path and proved nothing. Each HAVING below carries a GROUP BY for
/// that reason.
#[test]
fn a_malformed_escape_is_refused_even_where_no_row_evaluates_it() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE LE (id INT NOT NULL, s VARCHAR(30) NULL)",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE LF (id INT NOT NULL, s VARCHAR(30) NULL)",
    );

    for sql in [
        // An empty table, through each clause that reaches an expression.
        "SELECT s LIKE 'a' ESCAPE '!!' FROM LE",
        "SELECT id FROM LE WHERE s LIKE 'a' ESCAPE '!!'",
        "SELECT id FROM LE ORDER BY s LIKE 'a' ESCAPE '!!'",
        // `GROUP BY s` is what makes this one bite. Without it the HAVING is
        // a bare aggregate, which runs over one implicit group even with no
        // rows — so it is caught by the per-row path and passes with every
        // resolve-time check removed, which is exactly a test that does not
        // bite. With a GROUP BY there are no groups, and nothing settles the
        // clause but the guard.
        "SELECT 1 FROM LE GROUP BY s HAVING MAX(s) LIKE 'a' ESCAPE '!!'",
        "SELECT s, COUNT(*) FROM LE GROUP BY s HAVING s LIKE 'a' ESCAPE '!!'",
        "SELECT COUNT(*) FROM LE GROUP BY s LIKE 'a' ESCAPE '!!'",
        // No aggregate and no GROUP BY, so this one is not grouped at all —
        // the clause is settled per row on the projection path, and over an
        // empty table there is no row to settle it. It reaches a different
        // guard from the two above and needs a case of its own.
        "SELECT s FROM LE HAVING s LIKE 'a' ESCAPE '!!'",
        // A join produces no pair of rows when either side is empty, so an ON
        // clause is settled for no row at all.
        "SELECT a.id FROM LE a JOIN LF b ON a.s LIKE b.s ESCAPE '!!'",
        "SELECT a.id FROM LE a LEFT JOIN LF b ON a.s LIKE b.s ESCAPE '!!'",
        // One row, and the LIKE still never evaluated.
        "SELECT CASE WHEN 0 THEN ('a' LIKE 'b' ESCAPE '!!') ELSE 9 END",
        "SELECT 0 AND ('a' LIKE 'b' ESCAPE '!!')",
        // A FROM-less HAVING, where the one implicit row would otherwise
        // settle the clause — so the branch nothing takes is what makes this
        // reach the guard rather than the evaluation.
        "SELECT 1 HAVING CASE WHEN 0 THEN ('a' LIKE 'b' ESCAPE '!!') ELSE 9 END > 0",
        // The two paths that sort an already-projected result. A union over
        // two empty sides settles its ORDER BY for no row at all, and the
        // non-empty union raises 1210 through the evaluation — so this
        // divergence is invisible until the result is empty. The FROM-less
        // form has its one row, and orders it by a clause the evaluator would
        // have read; the guard has to be reached before that.
        "SELECT s FROM LE UNION SELECT s FROM LF ORDER BY s LIKE 'a' ESCAPE '!!'",
        "SELECT 1 AS one ORDER BY one LIKE 'a' ESCAPE '!!'",
    ] {
        // Matched rather than unwrapped, so a statement that succeeds names
        // itself. Half of these differ from their neighbours by one clause,
        // and `unwrap_err` on the wrong one panics with a result set and no
        // hint of which statement produced it.
        let err = refusal_of(&engine, &mut session, sql);
        assert_eq!(err.code, 1210, "expected ER_WRONG_ARGUMENTS for: {sql}");
    }

    // A write is the worse half: `Affected { rows: 0 }` is what a WHERE that
    // matched nothing looks like too, so the caller cannot tell an illegal
    // statement from an empty one. Kept apart from the queries above because
    // the failure mode is, and because a write that got through would leave
    // the table changed for everything after it.
    for sql in [
        "UPDATE LE SET id = 2 WHERE s LIKE 'a' ESCAPE '!!'",
        "DELETE FROM LE WHERE s LIKE 'a' ESCAPE '!!'",
        "UPDATE LE SET s = CASE WHEN 0 THEN ('a' LIKE 'b' ESCAPE '!!') ELSE 'z' END",
        "UPDATE LE a JOIN LF b ON a.id = b.id SET a.s = 'x' \
         WHERE a.s LIKE 'a' ESCAPE '!!'",
        "DELETE a FROM LE a JOIN LF b ON a.id = b.id WHERE a.s LIKE 'a' ESCAPE '!!'",
    ] {
        let err = refusal_of(&engine, &mut session, sql);
        assert_eq!(err.code, 1210, "expected ER_WRONG_ARGUMENTS for: {sql}");
    }

    // A missing table is still answered first, as on 8.3 — the guard is after
    // the table is resolved and not before it.
    let err = engine
        .execute(
            &mut session,
            "DELETE FROM nosuch WHERE s LIKE 'a' ESCAPE '!!'",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1146, "1146 is resolved before 1210");

    // A name nothing answers to wins over it, as on 8.3.
    let err = engine
        .execute(
            &mut session,
            "SELECT nope LIKE 'x' ESCAPE '!!' FROM LE",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1054, "1054 is resolved before 1210");

    // The same shapes with a legal clause still run, so the guard refuses the
    // clause rather than the construct.
    assert!(rows(
        &engine,
        &mut session,
        "SELECT id FROM LE ORDER BY s LIKE 'a!%' ESCAPE '!'"
    )
    .is_empty());
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CASE WHEN 0 THEN ('a' LIKE 'b' ESCAPE '!') ELSE 9 END"
        ),
        vec![vec!["9"]]
    );
}

/// A prepare that cannot succeed has to fail at prepare.
///
/// `describe` runs the statement as a probe over no rows and swallows what the
/// probe itself caused, keeping only the refusals that are the statement's
/// fault. 1210 is one: no row and no bound argument can make `ESCAPE '!!'`
/// name one character. Without it in `refused_at_prepare` the probe raised the
/// error, the filter said no, and COM_STMT_PREPARE succeeded describing zero
/// columns where MySQL fails the prepare.
#[test]
fn a_malformed_escape_fails_the_prepare_rather_than_describing_nothing() {
    let (engine, session) = engine();

    let err = engine
        .describe(&session, "SELECT 'a' LIKE 'b' ESCAPE '!!'", 0)
        .unwrap_err();
    assert_eq!(err.code, 1210);

    // The legal clause still describes its one column, so the code above is
    // not refusing every LIKE.
    let described = engine
        .describe(&session, "SELECT 'a%' LIKE 'a!%' ESCAPE '!'", 0)
        .unwrap()
        .expect("a legal ESCAPE still describes");
    assert_eq!(described.len(), 1);
}

/// A binary ESCAPE is counted and read as bytes, or refused.
///
/// `as_text` on a binary string decodes lossily, which made `x'FF'` and
/// `x'FE'` the same one-character escape — U+FFFD, which neither of them names
/// and no pattern can hold — and let `x'C3A9'` through as one character where
/// 8.3 counts two. Measured on 8.3: `x'21'` is `!`, `x''` is off, `x'C3A9'`
/// and `x'2121'` are 1210, and `x'FF'` escapes with the byte 0xFF.
///
/// The last of those this engine cannot do — it matches over `char`s, both
/// operands having come through `as_text` — so it refuses rather than
/// escaping with something else. #0134 is the collation work that would fix it.
#[test]
fn a_binary_escape_is_measured_in_bytes() {
    let (engine, mut session) = engine();

    let accepted = rows(
        &engine,
        &mut session,
        "SELECT 'x%' LIKE 'x!%' ESCAPE x'21', 'a%' LIKE 'a!%' ESCAPE x'', \
         'a%' LIKE 'a%' ESCAPE x''",
    );
    assert_eq!(accepted, vec![vec!["1", "0", "1"]]);

    for sql in [
        "SELECT 'a%' LIKE 'a!%' ESCAPE x'C3A9'",
        "SELECT 'a%' LIKE 'a!%' ESCAPE x'2121'",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1210, "two bytes is two characters, for: {sql}");
    }

    // Refused, not answered wrongly, and not silently equal to `x'FE'`.
    for sql in [
        "SELECT 'a%' LIKE 'a!%' ESCAPE x'FF'",
        "SELECT 'a%' LIKE 'a!%' ESCAPE x'FE'",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1235, "expected a refusal for: {sql}");
    }
}

/// A NULL ESCAPE makes the column nullable without making any answer NULL.
///
/// The two halves disagree and both are 8.3's: `'a%' LIKE 'a\%' ESCAPE NULL`
/// is 1 and never NULL, because a NULL escape leaves the default backslash
/// alone — and the column still comes back without NOT_NULL, where the same
/// LIKE with `ESCAPE ''` or with no clause at all carries it. Found by the
/// differential corpus; reasoning from the values says the opposite.
#[test]
fn a_null_escape_clause_makes_a_like_nullable_without_making_it_null() {
    let (engine, mut session) = engine();

    let not_null: Vec<bool> = engine
        .describe(
            &session,
            r"SELECT 'a%' LIKE 'a\%' ESCAPE NULL, 'a%' LIKE 'a\%' ESCAPE '',
              'a%' LIKE 'a\%', 'a%' NOT LIKE 'a\%' ESCAPE NULL",
            0,
        )
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| !c.nullable)
        .collect();
    assert_eq!(not_null, vec![false, true, true, false]);

    let answers = rows(
        &engine,
        &mut session,
        r"SELECT 'a%' LIKE 'a\%' ESCAPE NULL, 'a%' NOT LIKE 'a\%' ESCAPE NULL",
    );
    assert_eq!(answers, vec![vec!["1", "0"]], "the value is never NULL");
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

    // And is described as a connective rather than as a concatenation: MySQL
    // calls `1 || 1` a LONGLONG and NOT NULL. Reading the token one way
    // whatever the mode said announced VAR_STRING and nullable over an answer
    // of 1, which the binary protocol sends by the announced type.
    let described = engine
        .describe(&session, "SELECT 1 || 1", 0)
        .unwrap()
        .unwrap();
    assert_eq!(format!("{:?}", described[0].ty), "LongLong");
    assert!(!described[0].nullable);

    // Which it means also decides how tightly it binds, and therefore what a
    // `DIV` beside it divides. As OR it is looser, so the divisor comes out of
    // it and this is `(7 DIV 2) || 3`.
    assert_eq!(
        rows(&engine, &mut session, "SELECT 7 DIV 2 || 3"),
        vec![vec!["1"]]
    );

    // Looser than `AND` too, and by the same measurement: `(1 || 1) AND 0` is
    // 0 and MySQL says 1.
    assert_eq!(
        rows(&engine, &mut session, "SELECT 1 || 1 AND 0"),
        vec![vec!["1"]]
    );

    session.config.sql_mode.push_str(",PIPES_AS_CONCAT");
    let as_concat = rows(&engine, &mut session, "SELECT 'a' || 'b'");
    assert_eq!(as_concat, vec![vec!["ab"]]);

    // Concatenating, it binds tighter than `DIV`, so the same text is
    // `7 DIV '23'` — 0. The tree is identical either way; only the mode says
    // which shape it should have.
    assert_eq!(
        rows(&engine, &mut session, "SELECT 7 DIV 2 || 3"),
        vec![vec!["0"]]
    );
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

/// Under REAL_AS_FLOAT a `REAL` column is four bytes rather than eight.
///
/// The mode the target deployment runs sets this flag, so `REAL` there is not
/// the `REAL` of a default server. Every row below measured on MySQL 8.3 under
/// both modes.
#[test]
fn real_as_float_narrows_what_real_declares() {
    let (engine, mut session) = engine();

    // Without the flag `REAL` is `DOUBLE`, which is MySQL's default.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Wide (r REAL, ru REAL UNSIGNED, dp DOUBLE PRECISION, \
         dpu DOUBLE PRECISION UNSIGNED)",
    );

    session.config.sql_mode.push_str(",REAL_AS_FLOAT");
    run(
        &engine,
        &mut session,
        "CREATE TABLE Narrow (r REAL, ru REAL UNSIGNED, dp DOUBLE PRECISION, \
         dpu DOUBLE PRECISION UNSIGNED)",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, NUMERIC_PRECISION \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME IN ('Narrow', 'Wide') \
             ORDER BY TABLE_NAME, ORDINAL_POSITION"
        ),
        vec![
            vec!["Narrow", "r", "float", "float", "12"],
            vec!["Narrow", "ru", "float", "float unsigned", "12"],
            // `DOUBLE PRECISION` is not a synonym for `REAL`, and the flag
            // leaves it and its unsigned form alone.
            vec!["Narrow", "dp", "double", "double", "22"],
            vec!["Narrow", "dpu", "double", "double unsigned", "22"],
            vec!["Wide", "r", "double", "double", "22"],
            vec!["Wide", "ru", "double", "double unsigned", "22"],
            vec!["Wide", "dp", "double", "double", "22"],
            vec!["Wide", "dpu", "double", "double unsigned", "22"],
        ]
    );

    // The declared width is what the value is held in, so the narrow column
    // loses the digits four bytes cannot carry — and the table created before
    // the flag keeps every one of them, because the type was fixed by the DDL
    // and not by the mode in force when it is read.
    run(
        &engine,
        &mut session,
        "INSERT INTO Narrow (r) VALUES (1.234567890123)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Wide (r) VALUES (1.234567890123)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT r FROM Narrow"),
        vec![vec!["1.23457"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT r FROM Wide"),
        vec![vec!["1.234567890123"]]
    );

    // And it says so on the wire: the binary protocol encodes by the type
    // code, so a column announced `Double` is sent as eight bytes whatever it
    // was declared.
    let described = engine
        .describe(&session, "SELECT Narrow.r, Wide.r FROM Narrow, Wide", 0)
        .unwrap()
        .unwrap();
    let codes: Vec<String> = described.iter().map(|c| format!("{:?}", c.ty)).collect();
    assert_eq!(codes, vec!["Float", "Double"]);
}

/// `ANSI` is the compound spelling, and it has to mean all five flags.
///
/// The expansion was short by `REAL_AS_FLOAT` and `ONLY_FULL_GROUP_BY`, which
/// nothing could see while the first was inert. Both halves measured on 8.3
/// under `SET sql_mode = 'ANSI'`.
#[test]
fn the_compound_ansi_mode_narrows_a_real_and_polices_grouping() {
    let (engine, mut session) = engine();
    session.config.sql_mode = "ANSI".to_owned();

    run(
        &engine,
        &mut session,
        "CREATE TABLE A (id INT NOT NULL, r REAL NULL, dp DOUBLE PRECISION NULL, \
         PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO A VALUES (1, 1.234567890123, 1.234567890123)",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, NUMERIC_PRECISION \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'A' ORDER BY ORDINAL_POSITION"
        ),
        vec![
            vec!["id", "int", "int", "10"],
            // The flag the compound spelling was missing.
            vec!["r", "float", "float", "12"],
            vec!["dp", "double", "double", "22"],
        ]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT r, dp FROM A"),
        vec![vec!["1.23457", "1.234567890123"]]
    );

    // And the other one it was missing. `v` is not functionally determined by
    // `k` here, which is what separates this from the primary-key case MySQL
    // allows even under the flag.
    run(&engine, &mut session, "CREATE TABLE G2 (k INT, v INT)");
    run(&engine, &mut session, "INSERT INTO G2 VALUES (1,10),(1,20)");
    let refused = engine
        .execute(&mut session, "SELECT k, v FROM G2 GROUP BY k", &[])
        .expect_err("ANSI implies ONLY_FULL_GROUP_BY");
    assert_eq!(refused.code, 1055);
}

/// A cast follows the same flag the declaration does.
#[test]
fn real_as_float_narrows_a_cast_to_real() {
    let (engine, mut session) = engine();

    assert_eq!(
        rows(&engine, &mut session, "SELECT CAST(1.234567890123 AS REAL)"),
        vec![vec!["1.234567890123"]]
    );

    session.config.sql_mode.push_str(",REAL_AS_FLOAT");
    assert_eq!(
        rows(&engine, &mut session, "SELECT CAST(1.234567890123 AS REAL)"),
        vec![vec!["1.23457"]],
        "the value has to be the one the four bytes hold"
    );

    // The description has to move with the value: MySQL types this cast FLOAT
    // under the flag and DOUBLE without it.
    let described = engine
        .describe(&session, "SELECT CAST(1.234567890123 AS REAL)", 0)
        .unwrap()
        .unwrap();
    assert_eq!(format!("{:?}", described[0].ty), "Float");

    // What the narrowing refuses, which is the half the digits above cannot
    // see: the projection rounds a value to the type it announced either way,
    // so only an operand that four bytes cannot hold at all separates a cast
    // that narrowed from one that was merely described as if it had. MySQL
    // errors here under the flag and answers 1e300 without it, and it names
    // the cast `as float` even where the statement wrote REAL.
    let refused = engine
        .execute(&mut session, "SELECT CAST(1e300 AS REAL)", &[])
        .expect_err("four bytes cannot hold 1e300");
    assert_eq!(refused.code, 1690);
    assert_eq!(
        refused.message,
        "DOUBLE value is out of range in 'cast(1e300 as float)'"
    );

    session.config.sql_mode = ServerConfig::default().sql_mode;
    assert_eq!(
        rows(&engine, &mut session, "SELECT CAST(1e300 AS REAL)"),
        vec![vec!["1e300"]],
        "without the flag the cast is to a double and holds it"
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
    // Four places, because a quotient is four wider than its left operand:
    // `SUM(amount)` over an INT is a DECIMAL with no places, and MySQL answers
    // this 150.0000 and 50.0000 on 8.3.
    assert_eq!(
        averaged,
        vec![vec!["north", "150.0000"], vec!["south", "50.0000"]]
    );
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
        // 90.0000 rather than 90: a quotient is four places wider than its
        // left operand, which is `div_precision_increment`.
        vec![vec!["90.0000"]]
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

/// The table the HAVING tests below read, and one with nothing in it.
fn having_rows(engine: &Engine, session: &mut Session) {
    run(
        engine,
        session,
        "CREATE TABLE H (id INT NOT NULL, b INT NULL, s VARCHAR(10) NOT NULL, PRIMARY KEY (id))",
    );
    run(
        engine,
        session,
        "INSERT INTO H VALUES (1,1,'x'),(2,2,'y'),(3,3,'z'),(4,NULL,'w')",
    );
    run(
        engine,
        session,
        "CREATE TABLE HE (id INT NOT NULL, b INT NULL, PRIMARY KEY (id))",
    );
}

/// A HAVING with no aggregate does not group the query.
///
/// Measured on 8.3: `SELECT b FROM H HAVING b > 1` answers the two rows where
/// `b` is greater than 1, and describes the column exactly as `SELECT b FROM
/// H` does — same type, same width, same flags, same origin table. The clause
/// is a second filter there, not a grouping.
///
/// This engine read the presence of the clause as making the query grouped, so
/// an ordinary column became a nonaggregated reference and ONLY_FULL_GROUP_BY
/// refused every statement here. Refusing what MySQL answers is the direction
/// that breaks working queries rather than merely disagreeing with them.
#[test]
fn a_having_without_an_aggregate_does_not_group_the_query() {
    let (engine, mut session) = engine();
    having_rows(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT b FROM H HAVING b > 1 ORDER BY b"
        ),
        vec![vec!["2"], vec!["3"]]
    );
    // Every column stays available, which is the part grouping would have
    // taken away: `SELECT *` is not even a legal select item in a grouped
    // query here.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT * FROM H HAVING b > 1 ORDER BY id"
        ),
        vec![vec!["2", "2", "y"], vec!["3", "3", "z"]]
    );
    // The clause is settled per row and after the projection, so it can name
    // an output alias and still filter row by row.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT b + 1 AS bp FROM H HAVING bp > 2 ORDER BY bp"
        ),
        vec![vec!["3"], vec!["4"]]
    );
    // Ahead of DISTINCT, behind WHERE, and ahead of ORDER BY and LIMIT.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DISTINCT s FROM H WHERE id < 4 HAVING s > 'w' ORDER BY s LIMIT 2"
        ),
        vec![vec!["x"], vec!["y"]]
    );
    // An aggregate inside a subquery belongs to that subquery. Grouping the
    // outer query for it would collapse four rows into one.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT b FROM H HAVING b > (SELECT MIN(b) FROM H) ORDER BY b"
        ),
        vec![vec!["2"], vec!["3"]]
    );
    // Over no rows it produces no rows, where the same clause with an
    // aggregate in it settles over one implicit group. Both measured on 8.3.
    assert!(rows(&engine, &mut session, "SELECT b FROM HE HAVING b > 1").is_empty());
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM HE HAVING COUNT(*) >= 0"
        ),
        vec![vec!["0"]]
    );
}

/// A window function is computed over the rows the HAVING kept.
///
/// MySQL evaluates a window *after* HAVING, so the clause decides what the
/// window even sees. Measured on 8.3 over H(1,2,3,NULL): `HAVING b > 1` leaves
/// two rows, `ROW_NUMBER()` numbers them 1 and 2, `COUNT(*) OVER ()` answers 2
/// and `SUM(id) OVER ()` answers 5 — not 2 and 3, 4, and 10.
///
/// The first arrangement of this fix filtered the projected rows, which meant
/// `project` had already computed every window over every row the WHERE kept.
/// The row *set* was right and the window values were wrong, which is the
/// worse failure: these statements were refused with 1055 before the clause
/// stopped grouping, so a refusal had become a wrong answer.
///
/// There is no circularity to worry about. A HAVING can never name a window,
/// directly or through an alias — that is 3593 and 3594 — so nothing the
/// clause reads depends on a value that is not yet computed.
#[test]
fn a_window_is_computed_after_an_ungrouped_having() {
    let (engine, mut session) = engine();
    having_rows(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT b, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM H HAVING b > 1 ORDER BY b"
        ),
        vec![vec!["2", "1"], vec!["3", "2"]]
    );
    // The row set is the same under either order and only the window value
    // tells them apart, which is why this asserts the value: 2 is the count of
    // what survived and 4 is the count of what the WHERE left.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT b, COUNT(*) OVER () AS c FROM H HAVING b > 1 ORDER BY b"
        ),
        vec![vec!["2", "2"], vec!["3", "2"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT b, SUM(id) OVER () AS t FROM H HAVING b > 1 ORDER BY b"
        ),
        vec![vec!["2", "5"], vec!["3", "5"]]
    );
    // Partitions are drawn over the survivors too, not merely renumbered.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT b, ROW_NUMBER() OVER (PARTITION BY b > 2 ORDER BY id) AS rn FROM H \
             HAVING b IS NOT NULL ORDER BY b"
        ),
        vec![vec!["1", "1"], vec!["2", "2"], vec!["3", "1"]]
    );
    // LIMIT and DISTINCT come after the window, so neither narrows it.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT b, COUNT(*) OVER () AS c FROM H HAVING b > 1 ORDER BY b LIMIT 1"
        ),
        vec![vec!["2", "2"]]
    );
    assert!(rows(
        &engine,
        &mut session,
        "SELECT b, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM H HAVING 0"
    )
    .is_empty());
}

/// An aggregate written in the HAVING and nowhere else still groups the query.
///
/// The other side of the boundary above, and the reason the routing asks what
/// the clause *contains* rather than whether it is there: `SELECT b FROM H
/// HAVING COUNT(*) > 0` is one implicit group covering every row on 8.3, so
/// `b` is a nonaggregated column in an aggregated query and the statement is
/// refused rather than answered.
#[test]
fn an_aggregate_in_the_having_groups_the_query() {
    let (engine, mut session) = engine();
    having_rows(&engine, &mut session);
    assert!(session.sql_mode().only_full_group_by);

    for sql in [
        "SELECT b FROM H HAVING COUNT(*) > 0",
        "SELECT b FROM H HAVING b > 1 AND COUNT(*) > 0",
        "SELECT b FROM H HAVING SUM(id) > 1",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1140, "{sql} -> {err}");
    }

    // And one that groups legally answers the single row the group produces.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM H HAVING COUNT(*) > 0"
        ),
        vec![vec!["4"]]
    );
}

/// An aggregated query with no GROUP BY is 1140, not 1055.
///
/// Two different complaints on 8.3: 1055 is a column missing from a GROUP BY
/// that exists, and ER_MIX_OF_GROUP_FUNC_AND_FIELDS_V2 at 1140 is one in a
/// query that has no GROUP BY to be missing from. This engine sent 1055 for
/// both, so a client branching on the code was told the wrong rule.
#[test]
fn an_aggregated_query_without_a_group_by_is_1140() {
    let (engine, mut session) = engine();
    having_rows(&engine, &mut session);

    let err = engine
        .execute(&mut session, "SELECT COUNT(*), b FROM H", &[])
        .unwrap_err();
    assert_eq!(err.code, 1140, "{err}");
    assert!(
        err.message.contains("without GROUP BY"),
        "expected 8.3's wording, got: {err}"
    );

    // With a GROUP BY it stays 1055.
    let err = engine
        .execute(&mut session, "SELECT id, COUNT(*) FROM H GROUP BY b", &[])
        .unwrap_err();
    assert_eq!(err.code, 1055, "{err}");
}

/// HAVING reads the select list and the grouping, not the FROM clause.
///
/// Measured on 8.3, and with ONLY_FULL_GROUP_BY off as well as on: a name in a
/// HAVING resolves against the select list, the GROUP BY columns and any
/// enclosing query — nothing else. `SELECT b FROM H HAVING id > 1` is 1054
/// "in 'having clause'" there with `id` sitting in `H`.
///
/// It is what keeps the per-row HAVING honest. That path makes every column of
/// the relation reachable from the clause, so without this `SELECT b FROM H
/// HAVING id > 1` would have become an answer — and `SELECT COUNT(*) FROM H
/// HAVING b > 1`, which this engine answered as an empty set because the
/// group's first row happened not to pass, would have stayed one.
#[test]
fn a_having_does_not_read_the_from_clause() {
    let (engine, mut session) = engine();
    having_rows(&engine, &mut session);

    for sql in [
        "SELECT b FROM H HAVING id > 1",
        "SELECT COUNT(*) FROM H HAVING b > 1",
        "SELECT b FROM H GROUP BY b HAVING id > 1",
        // The item's *name* is what a name can match, and `b + 1` is not `b`.
        "SELECT b + 1 FROM H HAVING b + 1 > 2",
        "SELECT UPPER(s) FROM H HAVING s > 'w'",
        // An aggregate's argument does read the relation, so this one is a
        // name nothing in the statement answers to at all.
        "SELECT SUM(id) FROM H HAVING SUM(nope) > 1",
        // Ahead of the ESCAPE guard, because 8.3 resolves a clause's names
        // before its arguments.
        "SELECT b FROM H HAVING nope LIKE 'a' ESCAPE '!!'",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1054, "{sql} -> {err}");
        assert!(
            err.message.contains("having clause"),
            "{sql} -> {err}: expected the clause to be named"
        );
    }

    // The field list is resolved first, which is the order 8.3 answers in and
    // is only true while that pass runs ahead of the filter.
    let err = engine
        .execute(&mut session, "SELECT nope FROM H HAVING zz > 1", &[])
        .unwrap_err();
    assert_eq!(err.code, 1054, "{err}");
    assert!(err.message.contains("field list"), "{err}");
    let err = engine
        .execute(&mut session, "SELECT a.* FROM H HAVING zz > 1", &[])
        .unwrap_err();
    assert_eq!(err.code, 1051, "{err}");
    // And the HAVING before the ORDER BY, also measured.
    let err = engine
        .execute(
            &mut session,
            "SELECT b FROM H HAVING nope > 1 ORDER BY alsonope",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1054, "{err}");
    assert!(err.message.contains("having clause"), "{err}");

    for sql in [
        // By identity rather than by spelling: neither name on this column is
        // the one the clause wrote.
        "SELECT b AS zzz FROM H HAVING b > 1",
        "SELECT b FROM H HAVING H.b > 1",
        // A column the query groups by, whether or not it is also selected.
        "SELECT COUNT(*) FROM H GROUP BY s HAVING s > 'w'",
        // An aggregate's argument reads the whole relation.
        "SELECT SUM(id) FROM H HAVING SUM(id) > 1",
        // An enclosing query's column: 8.3 resolves a HAVING name outwards.
        "SELECT id FROM H WHERE (SELECT MAX(x.b) FROM H x HAVING H.id > 0) IS NOT NULL",
    ] {
        run(&engine, &mut session, sql);
    }
}

/// A HAVING name that could mean two different things is 1052.
///
/// The per-row path would otherwise have answered with whichever the
/// substitution reached first, which is a value chosen by this engine's
/// internals: `SELECT id AS b, t.b FROM t HAVING b > 1` is
/// ER_NON_UNIQ_ERROR on 8.3. The same column written twice is *not* ambiguous
/// there, which is why the check compares what the names resolve to.
#[test]
fn a_having_name_meaning_two_things_is_refused() {
    let (engine, mut session) = engine();
    having_rows(&engine, &mut session);

    for sql in [
        "SELECT id AS b, H.b FROM H HAVING b > 1",
        "SELECT b, id AS b FROM H HAVING b > 1",
        // Through a `*`, where the select list never wrote either name: a
        // self-join projects two columns called `id`, and resolving to the
        // first is an answer chosen by which side of the join came first.
        "SELECT * FROM H a JOIN H c ON a.id = c.id HAVING id > 1",
        "SELECT a.*, c.b FROM H a JOIN H c ON a.id = c.id HAVING b > 1",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1052, "{sql} -> {err}");
    }

    // Twice over is one thing, and answers. So is a qualified `*`, which
    // projects one of the two.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT H.b, b FROM H HAVING b > 1 ORDER BY 1"
        ),
        vec![vec!["2", "2"], vec!["3", "3"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT a.id FROM H a JOIN H c ON a.id = c.id HAVING id > 1 ORDER BY 1"
        ),
        vec![vec!["2"], vec!["3"], vec!["4"]]
    );
}

/// A window function cannot be read from a HAVING, directly or through an
/// alias.
///
/// A window is computed over the finished result, so a clause deciding what
/// the result contains cannot use one: 8.3 answers 3593 for the function and
/// 3594 for an alias of an expression containing one. The alias half is the
/// one the per-row path would have got wrong rather than merely miscoded — the
/// projected row holds a real row number, so the clause would have filtered on
/// it and answered.
#[test]
fn a_window_function_in_a_having_is_refused() {
    let (engine, mut session) = engine();
    having_rows(&engine, &mut session);

    for sql in [
        "SELECT b FROM H HAVING ROW_NUMBER() OVER (ORDER BY id) > 1",
        "SELECT b FROM H GROUP BY b HAVING ROW_NUMBER() OVER () > 1",
        // Left to right through the clause, which is the order 8.3 refuses in.
        "SELECT b FROM H HAVING ROW_NUMBER() OVER () > 1 AND nope > 1",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 3593, "{sql} -> {err}");
    }

    for sql in [
        "SELECT b, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM H HAVING rn > 2",
        "SELECT b, ROW_NUMBER() OVER (ORDER BY b) AS rn FROM H GROUP BY b HAVING rn > 1",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 3594, "{sql} -> {err}");
    }

    // The name beside it still wins when it comes first.
    let err = engine
        .execute(
            &mut session,
            "SELECT b FROM H HAVING nope > 1 AND ROW_NUMBER() OVER () > 1",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1054, "{err}");
}

/// A FROM-less SELECT has a HAVING too, and this engine dropped it.
///
/// `SELECT 1 HAVING 0` is an empty result on 8.3 and was a row here, and
/// `SELECT 1 HAVING nope > 0` is 1054 there and was a row here as well. The
/// one implicit row is the group, so an aggregate in the clause folds over it
/// exactly as one in the select list already does.
#[test]
fn a_from_less_select_applies_its_having() {
    let (engine, mut session) = engine();

    assert!(rows(&engine, &mut session, "SELECT 1 HAVING 0").is_empty());
    assert_eq!(
        rows(&engine, &mut session, "SELECT 1 AS x HAVING x > 0"),
        vec![vec!["1"]]
    );
    assert!(rows(&engine, &mut session, "SELECT COUNT(*) HAVING COUNT(*) > 5").is_empty());

    // The clause is named as well as the code, because the per-row evaluation
    // this path already did raises the same 1054 under 'field list' — so a
    // test reading only the code passes with the resolve-time check removed.
    let err = engine
        .execute(&mut session, "SELECT 1 HAVING nope > 0", &[])
        .unwrap_err();
    assert_eq!(err.code, 1054, "{err}");
    assert!(err.message.contains("having clause"), "{err}");
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

    // An unsigned cast of a negative value is a reinterpretation, which the
    // engine could not represent until #0069 gave `Value` an unsigned tag —
    // it answered 1235 before that. All four measured on 8.3.
    //
    // The string is here because the reading has to be exact to reach these
    // numbers at all: read through an `i64` it saturated at
    // 9223372036854775807.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST(-1 AS UNSIGNED), CAST(-2 AS UNSIGNED), \
                    CAST('18446744073709551615' AS UNSIGNED), CAST('abc' AS UNSIGNED)"
        ),
        vec![vec![
            "18446744073709551615",
            "18446744073709551614",
            "18446744073709551615",
            "0"
        ]]
    );
}

/// A table of every unsigned width beside its signed counterparts.
fn unsigned_widths(engine: &Engine, session: &mut Session) {
    run(
        engine,
        session,
        "CREATE TABLE U (id INT NOT NULL, tu TINYINT UNSIGNED NULL, su SMALLINT UNSIGNED NULL, \
         mu MEDIUMINT UNSIGNED NULL, iu INT UNSIGNED NULL, bu BIGINT UNSIGNED NULL, \
         i INT NULL, b BIGINT NULL, PRIMARY KEY (id))",
    );
    run(
        engine,
        session,
        "INSERT INTO U VALUES (1, 7, 7, 7, 7, 7, 7, 7)",
    );
}

#[test]
fn unsigned_arithmetic_that_leaves_the_range_is_refused() {
    // The engine held an `i64` and answered `iu - 100` with -93, where MySQL
    // raises 1690 — which is why #0064 had to leave the UNSIGNED flag off a
    // subtraction. All measured on 8.3.
    let (engine, mut session) = engine();
    unsigned_widths(&engine, &mut session);

    for sql in [
        // Every width, so it is the column's sign deciding and not its size.
        "SELECT tu - 100 FROM U",
        "SELECT su - 100 FROM U",
        "SELECT mu - 100 FROM U",
        "SELECT iu - 100 FROM U",
        "SELECT bu - 100 FROM U",
        // Wider than subtraction, which is the whole of the issue's note: a
        // negative operand does it to `+`, `*` and `DIV` as well.
        "SELECT iu + -100 FROM U",
        "SELECT iu * -1 FROM U",
        "SELECT iu DIV -1 FROM U",
        // The unsigned operand on the right instead.
        "SELECT -100 + iu FROM U",
        "SELECT -1 * iu FROM U",
        "SELECT 0 - iu FROM U",
        "SELECT 6 - iu FROM U",
        "SELECT -8 DIV iu FROM U",
        // The chain, which is the case that settles where the check goes:
        // MySQL refuses this though 107 would fit, so the check is per
        // operator and not at the boundary #0052 moved the bitwise one to.
        "SELECT iu - 100 + 200 FROM U",
        "SELECT iu + 100 - 200 FROM U",
    ] {
        let error = engine.execute(&mut session, sql, &[]).expect_err(sql);
        assert_eq!(error.code, 1690, "{sql}");
        assert_eq!(error.sql_state, "22003", "{sql}");
        assert_eq!(
            error.message, "BIGINT UNSIGNED value is out of range",
            "{sql}"
        );
    }

    // A string operand takes the whole thing out of the integer domain, so it
    // answers where the same number written as a cast refuses. Both measured
    // on 8.3, and the pair is why the operand test reads the tag rather than
    // asking whether the value happens to be a number.
    assert_eq!(
        rows(&engine, &mut session, "SELECT iu - '100' FROM U"),
        vec![vec!["-93"]]
    );
    let error = engine
        .execute(
            &mut session,
            "SELECT iu - CAST('100' AS SIGNED) FROM U",
            &[],
        )
        .expect_err("a cast is an integer operand");
    assert_eq!(error.code, 1690, "{}", error.message);

    // And everything whose true value stays inside the range still answers,
    // negative operands included: it is the answer that is checked and not the
    // spelling of the operands.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT iu - 7, iu - -1, 100 - iu, 7 - iu, iu + -7, iu * 0 FROM U"
        ),
        vec![vec!["0", "8", "93", "0", "0", "0"]]
    );
}

#[test]
fn modulo_takes_its_sign_from_its_left_operand_alone() {
    // The one arithmetic operator that does not read both sides. Measured on
    // 8.3: `iu MOD -1` is UNSIGNED and answers 0, `-1 MOD iu` is signed and
    // answers -1. Reading either side would have made the second one unsigned
    // and refused it, since -1 is not in the range.
    let (engine, mut session) = engine();
    unsigned_widths(&engine, &mut session);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT iu MOD -1, iu MOD -3, -1 MOD iu, -1 DIV iu FROM U"
        ),
        vec![vec!["0", "1", "-1", "0"]]
    );

    let signs: Vec<bool> = engine
        .describe(&session, "SELECT iu MOD -1, -1 MOD iu FROM U", 0)
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| c.unsigned)
        .collect();
    assert_eq!(signs, vec![true, false]);
}

#[test]
fn signed_arithmetic_that_leaves_the_range_is_refused_too() {
    // The engine wrapped these deliberately, on the strength of a comment
    // saying MySQL lets a sum wrap. It does not: measured on 8.3, all four
    // raise 1690 with the *signed* spelling of the message, which is how a
    // client tells which domain overflowed.
    let (engine, mut session) = engine();
    unsigned_widths(&engine, &mut session);

    for sql in [
        "SELECT b + 9223372036854775807 FROM U",
        "SELECT b * 9223372036854775807 FROM U",
        "SELECT 9223372036854775807 + 1",
        "SELECT -9223372036854775808 - 1",
    ] {
        let error = engine.execute(&mut session, sql, &[]).expect_err(sql);
        assert_eq!(error.code, 1690, "{sql}");
        assert_eq!(error.message, "BIGINT value is out of range", "{sql}");
    }
}

#[test]
fn a_bigint_unsigned_column_holds_its_whole_range() {
    // The engine capped the column at `i64::MAX` and refused the top half of
    // what it can hold, which was the other half of holding only an `i64`.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE W (id INT NOT NULL, bu BIGINT UNSIGNED NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO W VALUES (1, 18446744073709551615)",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT bu, bu - 1, bu + -1, bu * 1, bu - bu, bu DIV 10 FROM W"
        ),
        vec![vec![
            "18446744073709551615",
            "18446744073709551614",
            "18446744073709551614",
            "18446744073709551615",
            "0",
            "1844674407370955161"
        ]]
    );

    // Comparison at the top of the range, which the `f64` reading could not
    // do: every `u64` above 2^53 rounds, so these two literals were one
    // number and the first comparison answered 1 for both.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT bu = 18446744073709551615, bu = 18446744073709551614, \
                    bu > 9223372036854775807, bu > -1 FROM W"
        ),
        vec![vec!["1", "0", "1", "1"]]
    );

    // A rounding at a non-negative scale hands the number back rather than
    // finding out through an `f64` that it was already whole: the double
    // rounds 18446744073709551615 up to 2^64, which is not a value the column
    // can hold at all.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT ROUND(bu), TRUNCATE(bu, 0), FLOOR(bu), CEIL(bu) FROM W"
        ),
        vec![vec![
            "18446744073709551615",
            "18446744073709551615",
            "18446744073709551615",
            "18446744073709551615"
        ]]
    );

    // The same value as a *bound parameter*, which is the binary protocol's
    // way in. Read as an `i64` it arrived as -1 and the column refused it, so
    // a statement the text protocol accepted failed when a client prepared it.
    let top = Value::from_wire(&mysql_ultralight_protocol::Value::U64(u64::MAX));
    engine
        .execute(&mut session, "INSERT INTO W VALUES (2, ?)", &[top])
        .expect("a bound BIGINT UNSIGNED at the top of its range");
    assert_eq!(
        rows(&engine, &mut session, "SELECT bu FROM W WHERE id = 2"),
        vec![vec!["18446744073709551615"]]
    );
    // A parameter that fits signed is unchanged, which is the half that keeps
    // every statement that already worked working: a client sends most
    // integers unsigned whatever their size, and `? - 100` over a bound 7 is
    // -93 rather than a refusal.
    let small = Value::from_wire(&mysql_ultralight_protocol::Value::U64(7));
    let Outcome::Rows(bound) = engine
        .execute(&mut session, "SELECT ? - 100", &[small])
        .expect("a bound 7 is a signed 7")
    else {
        panic!("expected rows")
    };
    assert_eq!(bound.rows[0][0].as_text(), "-93");

    // A signed cast wraps rather than refusing, which is MySQL's answer for
    // the one value that has to choose.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST(bu AS SIGNED), CAST(bu AS CHAR) FROM W WHERE id = 1"
        ),
        vec![vec!["-1", "18446744073709551615"]]
    );

    // Above the range still refuses, at the column and at the operator.
    let error = engine
        .execute(
            &mut session,
            "INSERT INTO W VALUES (2, 18446744073709551616)",
            &[],
        )
        .expect_err("2^64 is not a BIGINT UNSIGNED");
    assert_eq!(error.code, 1264, "{}", error.message);
    let error = engine
        .execute(&mut session, "SELECT bu + 1 FROM W", &[])
        .expect_err("2^64 is not a BIGINT UNSIGNED");
    assert_eq!(error.code, 1690, "{}", error.message);

    // The protocol's own rendering, which is a second one: `rows` above reads
    // the engine's `as_text`, and a client reads this. Sent as an `i64` the
    // column would announce UNSIGNED and carry the digits of -1.
    let Outcome::Rows(result) = run(&engine, &mut session, "SELECT bu FROM W") else {
        panic!("expected rows")
    };
    let mut text = mysql_ultralight_protocol::BufWriter::new();
    result.rows[0][0].to_wire().encode_text(&mut text);
    assert_eq!(
        String::from_utf8_lossy(&text.into_inner()[1..]),
        "18446744073709551615"
    );

    // A sign drops the flag rather than keeping it, so this refuses in the
    // *signed* domain — MySQL's answer, and the message says which.
    let error = engine
        .execute(&mut session, "SELECT -bu FROM W", &[])
        .expect_err("-(2^64-1) is not a BIGINT");
    assert_eq!(error.message, "BIGINT value is out of range");
    // While a sign over a value that does fit answers, and goes on answering
    // once it is negative.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT -(bu DIV 10), -(bu DIV 10) - 100 FROM W WHERE id = 1"
        ),
        vec![vec!["-1844674407370955161", "-1844674407370955261"]]
    );
}

#[test]
fn an_else_is_a_candidate_for_the_sign_like_every_other_branch() {
    // The `ELSE` as the *only* unsigned candidate is the shape that reads it
    // and nothing else does — with an unsigned `THEN` the flag is decided
    // before the `ELSE` is looked at. Described signed while the value kept
    // its tag, the text protocol wrote every digit of 18446744073709551615 and
    // a binary client read the same eight bytes as -1.
    //
    // The three spellings are one expression, and MySQL calls all three
    // UNSIGNED on 8.3 — the siblings are here because they were already right,
    // so a fix that broke them would say so.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE W (id INT NOT NULL, bu BIGINT UNSIGNED NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO W VALUES (1, 18446744073709551615)",
    );

    let sql = "SELECT CASE WHEN 0 THEN NULL ELSE bu END, COALESCE(NULL, bu), \
               IF(0, NULL, bu) FROM W";
    assert_eq!(
        rows(&engine, &mut session, sql),
        vec![vec!["18446744073709551615"; 3]]
    );
    let signs: Vec<bool> = engine
        .describe(&session, sql, 0)
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| c.unsigned)
        .collect();
    assert_eq!(signs, vec![true, true, true]);

    // And a signed `ELSE` still gives it away, so the fix widened what is read
    // rather than what is accepted.
    let mixed: Vec<bool> = engine
        .describe(
            &session,
            "SELECT CASE WHEN 0 THEN NULL ELSE 1 END, CASE WHEN 1 THEN bu ELSE 1 END FROM W",
            0,
        )
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| c.unsigned)
        .collect();
    assert_eq!(mixed, vec![false, false]);
}

#[test]
fn a_table_value_constructor_describes_an_unsigned_value_as_an_integer() {
    // `to_wire` sends a `WireValue::U64`, so a column announced VAR_STRING —
    // which is where the catch-all put it — wrote eight raw bytes where a
    // binary client was reading a length-encoded string, and desynchronised
    // the row it was in the middle of. MySQL types `VALUES ROW(...)` over an
    // unsigned value LONGLONG UNSIGNED. The same defect the diff fixed for a
    // boolean by untagging before the columns are derived; an unsigned value
    // cannot be untagged, because MySQL really does describe it unsigned.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("VALUES ROW(18446744073709551615)", "LongLong true"),
        ("VALUES ROW(~0)", "LongLong true"),
        ("VALUES ROW(1 << 63)", "LongLong true"),
        ("VALUES ROW(1)", "LongLong false"),
        // A NULL row says nothing about the column.
        (
            "VALUES ROW(18446744073709551615), ROW(NULL)",
            "LongLong true",
        ),
        // Two rows sharing one column with no integer holding both, which is
        // where a mixed-sign union ends up too.
        (
            "VALUES ROW(1), ROW(18446744073709551615)",
            "NewDecimal false",
        ),
    ] {
        let described = engine.describe(&session, sql, 0).unwrap().unwrap();
        assert_eq!(
            format!("{:?} {}", described[0].ty, described[0].unsigned),
            expected,
            "{sql}"
        );
    }
    assert_eq!(
        rows(&engine, &mut session, "VALUES ROW(~0)"),
        vec![vec!["18446744073709551615"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "VALUES ROW(1), ROW(18446744073709551615)"
        ),
        vec![vec!["1"], vec!["18446744073709551615"]]
    );
}

#[test]
fn a_number_is_an_enum_index_whatever_its_sign() {
    // MySQL reads a number as a position and a string as a member, and the
    // signedness of the number is not part of that rule. Read as text, `1 & 1`
    // matched the *member* `1` while the literal 1 beside it stored the first
    // member `2` — the same number stored two different ways, decided by a tag
    // the statement never mentions. The declaration is out of alphabetical and
    // out of numeric order so an index and a member cannot pass for each other.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Idx (id INT NOT NULL, g ENUM('2','1') NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Idx VALUES (1, 1 & 1), (2, 1), (3, 2 & 2), (4, 2), \
         (5, 18446744073709551615 - 18446744073709551614)",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT g, g + 0 FROM Idx ORDER BY id"
        ),
        vec![
            vec!["2", "1"],
            vec!["2", "1"],
            vec!["1", "2"],
            vec!["1", "2"],
            vec!["2", "1"],
        ]
    );
    // A position no column declares is refused rather than wrapped, which is
    // what an index above `i64::MAX` becomes.
    let error = engine
        .execute(
            &mut session,
            "INSERT INTO Idx VALUES (6, 18446744073709551615)",
            &[],
        )
        .expect_err("2^64-1 is not a position in a two-member ENUM");
    assert_eq!(error.code, 1265, "{}", error.message);
}

#[test]
fn a_conditional_over_both_signs_widens_rather_than_wrapping() {
    // A `BIGINT UNSIGNED` beside a signed one has no 64-bit integer that holds
    // them both, so MySQL answers a NEWDECIMAL and keeps every digit of each.
    // Wrapping the tag off — which is what a signed `i64` reading does — made
    // all of these -1. Measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE M (id INT NOT NULL, bu BIGINT UNSIGNED NOT NULL, b BIGINT NOT NULL, \
         iu INT UNSIGNED NOT NULL, i INT NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO M VALUES (1, 18446744073709551615, 7, 7, 7)",
    );

    let sql = "SELECT COALESCE(bu, b), IFNULL(bu, b), GREATEST(bu, b), IF(1, bu, b), \
               CASE WHEN 1 THEN bu ELSE b END FROM M";
    assert_eq!(
        rows(&engine, &mut session, sql),
        vec![vec!["18446744073709551615"; 5]]
    );
    // And the code says so, or the binary protocol would read eight bytes of
    // something the text protocol spelled out in full.
    let codes: Vec<String> = engine
        .describe(&session, sql, 0)
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| format!("{:?} {}", c.ty, c.unsigned))
        .collect();
    assert_eq!(codes, vec!["NewDecimal false"; 5]);

    // A narrower pair still has an integer to widen into, so those stay
    // LONGLONG — the sign is what runs out, not the type system.
    let narrow: Vec<String> = engine
        .describe(
            &session,
            "SELECT COALESCE(iu, i), COALESCE(iu, b), COALESCE(iu, iu) FROM M",
            0,
        )
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| format!("{:?} {}", c.ty, c.unsigned))
        .collect();
    assert_eq!(
        narrow,
        vec!["LongLong false", "LongLong false", "Long true"]
    );

    // Three candidates, which is the only shape that reads the sign the fold
    // carries: with two, `widen` sees both declarations directly. A signed
    // `INT` and an `INT UNSIGNED` already need a `BIGINT` between them, and a
    // `BIGINT UNSIGNED` after that leaves nothing — MySQL says NEWDECIMAL, and
    // a fold that let either side's sign win would have called it a LONGLONG.
    let three: Vec<String> = engine
        .describe(
            &session,
            "SELECT COALESCE(i, iu, bu), COALESCE(i, iu) FROM M",
            0,
        )
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| format!("{:?} {}", c.ty, c.unsigned))
        .collect();
    assert_eq!(three, vec!["NewDecimal false", "LongLong false"]);
}

#[test]
fn a_rounding_at_a_negative_scale_stays_exact_over_an_unsigned_value() {
    // These compute in an `f64`, which has no digit left below 2^53 — so at
    // the top of a BIGINT UNSIGNED the answer was a number that had never been
    // in the column. Measured on 8.3: `TRUNCATE(bu, -1)` is
    // 18446744073709551610, and `ROUND(bu, -1)` *refuses*, because carrying up
    // leaves the range the answer has to fit in.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE W (id INT NOT NULL, bu BIGINT UNSIGNED NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO W VALUES (1, 18446744073709551615), (2, 18446744073709551605), \
         (3, 18446744073709551604)",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TRUNCATE(bu, -1), TRUNCATE(bu, -2), TRUNCATE(bu, -30) FROM W WHERE id = 1"
        ),
        vec![vec!["18446744073709551610", "18446744073709551600", "0"]]
    );
    // A scale so deep that the power of ten it cuts at does not fit either:
    // the answer is zero rather than an overflow, which is MySQL's too.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TRUNCATE(bu, -39), ROUND(bu, -39), TRUNCATE(bu, -100) FROM W WHERE id = 1"
        ),
        vec![vec!["0", "0", "0"]]
    );
    // Half goes up, which is measured rather than assumed.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT ROUND(bu, -1) FROM W WHERE id > 1 ORDER BY id"
        ),
        vec![vec!["18446744073709551610"], vec!["18446744073709551600"]]
    );
    let error = engine
        .execute(
            &mut session,
            "SELECT ROUND(bu, -1) FROM W WHERE id = 1",
            &[],
        )
        .expect_err("rounding up leaves the range");
    assert_eq!(error.code, 1690, "{}", error.message);
}

#[test]
fn a_conditional_keeps_the_unsigned_reading_only_where_every_candidate_has_one() {
    // The same all-candidates rule the UNSIGNED flag on the wire follows, and
    // it has to hold for the *value* as well or the two disagree row by row:
    // measured on 8.3, `COALESCE(iu, iu) - 100` raises 1690 and
    // `COALESCE(iu, 1) - 100` answers -93.
    let (engine, mut session) = engine();
    unsigned_widths(&engine, &mut session);

    for sql in [
        "SELECT COALESCE(iu, iu) - 100 FROM U",
        "SELECT IF(1, iu, iu) - 100 FROM U",
        "SELECT IFNULL(iu, iu) - 100 FROM U",
        "SELECT GREATEST(iu, iu) - 100 FROM U",
        "SELECT LEAST(iu, iu) - 100 FROM U",
        "SELECT (CASE WHEN 1 THEN iu ELSE iu END) - 100 FROM U",
        // A NULL candidate does not count against it, and a missing `ELSE` is
        // a NULL candidate.
        "SELECT COALESCE(NULL, iu) - 100 FROM U",
        "SELECT IF(1, iu, NULL) - 100 FROM U",
        "SELECT (CASE WHEN 1 THEN iu END) - 100 FROM U",
        // `NULLIF` answers its first argument and takes its sign from that
        // alone, which is why it is not in the list below.
        "SELECT NULLIF(iu, 1) - 100 FROM U",
        // A NULL branch beside the taken one does not count against it
        // either, where a `1` in the same position does — the pair below.
        "SELECT (CASE WHEN 0 THEN NULL WHEN 1 THEN iu END) - 100 FROM U",
    ] {
        let error = engine.execute(&mut session, sql, &[]).expect_err(sql);
        assert_eq!(error.code, 1690, "{sql}");
    }

    for sql in [
        "SELECT COALESCE(iu, 1) - 100 FROM U",
        "SELECT IF(1, iu, 1) - 100 FROM U",
        "SELECT IFNULL(iu, 1) - 100 FROM U",
        "SELECT GREATEST(iu, 1) - 100 FROM U",
        "SELECT (CASE WHEN 1 THEN iu ELSE 1 END) - 100 FROM U",
        "SELECT (CASE WHEN 0 THEN 1 ELSE iu END) - 100 FROM U",
        "SELECT (CASE WHEN 0 THEN 1 WHEN 1 THEN iu END) - 100 FROM U",
    ] {
        assert_eq!(rows(&engine, &mut session, sql), vec![vec!["-93"]], "{sql}");
    }
    // A branch MySQL never reaches cannot be what makes it refuse. `CASE` is
    // the one conditional whose candidates are not already evaluated, so the
    // engine evaluates the untaken ones to ask what they are — and this is the
    // case that says what to do when that fails: on 8.3 the whole expression
    // is UNSIGNED and raises 1690, because `bu + 1` is unsigned *as a type*
    // however the arithmetic in it would have gone.
    run(
        &engine,
        &mut session,
        "UPDATE U SET bu = 18446744073709551615 WHERE id = 1",
    );
    let error = engine
        .execute(
            &mut session,
            "SELECT (CASE WHEN 1 THEN iu ELSE bu + 1 END) - 100 FROM U",
            &[],
        )
        .expect_err("the untaken branch does not decide this");
    assert_eq!(error.code, 1690, "{}", error.message);

    // `LEAST(iu, 1)` picks the 1, so its answer differs from the six above
    // while the rule that produced it is the same one.
    assert_eq!(
        rows(&engine, &mut session, "SELECT LEAST(iu, 1) - 100 FROM U"),
        vec![vec!["-99"]]
    );
}

#[test]
fn the_unsigned_reading_survives_exactly_what_mysql_lets_it_survive() {
    // Measured on 8.3, and the list is its own: #0068's ENUM and #0061's
    // boolean each keep a different subset, and neither transfers. A scalar
    // subquery drops the ordinal and keeps this; a set operation drops all
    // three unless both branches agree, where a derived table keeps this and
    // the ordinal and drops the boolean.
    let (engine, mut session) = engine();
    unsigned_widths(&engine, &mut session);

    for sql in [
        "SELECT (SELECT iu FROM U) - 100",
        "SELECT x - 100 FROM (SELECT iu AS x FROM U) t",
        "SELECT x - 100 FROM (SELECT iu AS x FROM U UNION ALL SELECT iu FROM U) t",
        // `ABS`, `MIN` and `MAX` hand a row's own value back.
        "SELECT ABS(iu) - 100 FROM U",
        "SELECT MIN(iu) - 100 FROM U",
        "SELECT MAX(iu) - 100 FROM U",
    ] {
        let error = engine.execute(&mut session, sql, &[]).expect_err(sql);
        assert_eq!(error.code, 1690, "{sql}");
    }

    // A window function answers a row of its frame, or the default it was
    // given where it reaches past one — and the default is a candidate, so it
    // decides the sign along with the subject. Reached through a derived
    // table because a window function inside an expression is a separate gap:
    // `LAG(iu) OVER (...) - 100` is unsupported here whatever the sign.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Win (id INT NOT NULL, w INT UNSIGNED NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Win VALUES (1, 7), (2, 8)",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT x - 100 FROM (SELECT id, LAG(w, 1, 0) OVER (ORDER BY id) AS x FROM Win) t \
             ORDER BY x"
        ),
        vec![vec!["-100"], vec!["-93"]]
    );
    for sql in [
        "SELECT x - 100 FROM (SELECT id, LAG(w) OVER (ORDER BY id) AS x FROM Win) t ORDER BY x",
        "SELECT x - 100 FROM (SELECT id, FIRST_VALUE(w) OVER (ORDER BY id) AS x FROM Win) t \
         ORDER BY x",
    ] {
        let error = engine.execute(&mut session, sql, &[]).expect_err(sql);
        assert_eq!(error.code, 1690, "{sql}");
    }

    // A rounding hands back the number it was given rather than building a
    // new one, so all four keep the sign — the negative scale included, which
    // is the one that cannot answer by handing the argument straight back.
    for sql in [
        "SELECT ROUND(iu) - 100 FROM U",
        "SELECT CEIL(iu) - 100 FROM U",
        "SELECT FLOOR(iu) - 100 FROM U",
        "SELECT TRUNCATE(iu, 0) - 100 FROM U",
        "SELECT ROUND(iu, -1) - 100 FROM U",
        "SELECT TRUNCATE(iu, -1) - 100 FROM U",
    ] {
        let error = engine.execute(&mut session, sql, &[]).expect_err(sql);
        assert_eq!(error.code, 1690, "{sql}");
    }

    // A set operation with a signed branch drops it, which is the pair that
    // proves the union is not simply passing values through.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT x - 100 FROM (SELECT iu AS x FROM U UNION ALL SELECT i FROM U) t"
        ),
        vec![vec!["-93"], vec!["-93"]]
    );
    // `SUM`, `AVG` and `COUNT` build a new number and come back signed.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT SUM(iu) - 100, AVG(iu) - 100, COUNT(iu) - 100 FROM U"
        ),
        vec![vec!["-93", "-93.0000", "-99"]]
    );
}

#[test]
fn a_sum_over_an_unsigned_column_stays_exact_at_the_top_of_its_range() {
    // `SUM` and `AVG` build an exact total, and an unsigned operand had no
    // case there — so it fell through to the `f64` path and 18446744073709551615
    // came back as 18446744073709552000.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE W (id INT NOT NULL, bu BIGINT UNSIGNED NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO W VALUES (1, 18446744073709551615)",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT SUM(bu), AVG(bu), MIN(bu), MAX(bu) FROM W"
        ),
        vec![vec![
            "18446744073709551615",
            "18446744073709551615.0000",
            "18446744073709551615",
            "18446744073709551615"
        ]]
    );
}

#[test]
fn no_unsigned_subtraction_moves_the_domain_of_one_operator() {
    // The flag takes `-` out of the unsigned domain and takes nothing else out
    // of it, which is measured rather than read off its name.
    let (engine, mut session) = engine();
    unsigned_widths(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "SET SESSION sql_mode = CONCAT(@@sql_mode, ',NO_UNSIGNED_SUBTRACTION')",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT iu - 100, iu - 8, 6 - iu FROM U"
        ),
        vec![vec!["-93", "-1", "-1"]]
    );
    // And the flag on the wire moves with the value, or the text protocol
    // would say -93 while the binary one handed the same bytes to a client
    // reading 18446744073709551523.
    let signs: Vec<bool> = engine
        .describe(&session, "SELECT iu - 100, iu + 1 FROM U", 0)
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| c.unsigned)
        .collect();
    assert_eq!(signs, vec![false, true]);

    // What it does not touch.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT iu + 1, iu * 2, iu DIV 2 FROM U"
        ),
        vec![vec!["8", "14", "3"]]
    );
    for sql in [
        "SELECT iu + -100 FROM U",
        "SELECT iu * -1 FROM U",
        "SELECT iu DIV -1 FROM U",
    ] {
        let error = engine.execute(&mut session, sql, &[]).expect_err(sql);
        assert_eq!(error.code, 1690, "{sql}");
        assert_eq!(
            error.message, "BIGINT UNSIGNED value is out of range",
            "{sql}"
        );
    }

    // Turning it off again puts the subtraction back, so the mode is read per
    // statement rather than baked in when the column was.
    run(
        &engine,
        &mut session,
        "SET SESSION sql_mode = 'STRICT_TRANS_TABLES'",
    );
    let error = engine
        .execute(&mut session, "SELECT iu - 100 FROM U", &[])
        .expect_err("the mode is off again");
    assert_eq!(error.code, 1690);
}

#[test]
fn a_literal_above_the_signed_range_is_unsigned() {
    // MySQL types one BIGINT UNSIGNED, so it subtracts in the unsigned domain
    // and reports the flag. Read as a double the operand was rounded before
    // anything was taken off it.
    let (engine, mut session) = engine();
    assert_eq!(
        rows(&engine, &mut session, "SELECT 18446744073709551615 - 1"),
        vec![vec!["18446744073709551614"]]
    );
    let signs: Vec<bool> = engine
        .describe(
            &session,
            "SELECT 18446744073709551615 - 1, 9223372036854775807 - 1",
            0,
        )
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| c.unsigned)
        .collect();
    assert_eq!(signs, vec![true, false]);

    for sql in [
        "SELECT 18446744073709551615 + 1",
        "SELECT 0 - 18446744073709551615",
    ] {
        let error = engine.execute(&mut session, sql, &[]).expect_err(sql);
        assert_eq!(error.code, 1690, "{sql}");
    }

    // A bitwise operator reads an unsigned operand as the bit pattern it
    // already is. Through the `f64` fall-through the cast saturated and this
    // answered 9223372036854775807.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT 0 | 18446744073709551615, ~0 & 18446744073709551615"
        ),
        vec![vec!["18446744073709551615", "18446744073709551615"]]
    );
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
        .unwrap()
        .unwrap();
    assert_eq!(
        described.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        vec!["id", "name"]
    );

    // An alias is the name the client sees, and a parameter does not change
    // the shape — it is bound to NULL for the probe.
    let described = engine
        .describe(&session, "SELECT count AS n FROM Widget WHERE id = ?", 1)
        .unwrap()
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
            engine.describe(&session, sql, 0).unwrap().is_none(),
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
        .unwrap()
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
            // A DATE has no fractional seconds and MySQL reports NULL — not
            // 0, which is what a TIMESTAMP with none reports. This asserted 0
            // on the strength of a comment written from memory, and stayed
            // green until the differential harness asked MySQL.
            vec!["at", "NULL", "NULL", "NULL"],
        ]
    );
}

#[test]
fn column_type_is_spelled_the_way_mysql_spells_it() {
    // `COLUMN_TYPE` is not the DDL echoed back — it is rebuilt from the parsed
    // type, and every field it forgets is a silent difference from MySQL. Each
    // row here was read off MySQL 8.3 rather than reasoned about.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Shapes (
            big BIGINT UNSIGNED, wide INT(11), flag TINYINT(1),
            uflag TINYINT(1) UNSIGNED, money DECIMAL(10,2) UNSIGNED,
            vb VARBINARY(9), b BINARY(4), pick SET('a b','c''d'),
            one ENUM('p','q''r'), d DATE, ts TIMESTAMP NULL, dt DATETIME(3),
            approx FLOAT(7,3), big_approx DOUBLE UNSIGNED
        )",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, CHARACTER_MAXIMUM_LENGTH, \
                    NUMERIC_PRECISION, NUMERIC_SCALE, DATETIME_PRECISION \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'Shapes' \
             ORDER BY ORDINAL_POSITION"
        ),
        vec![
            // The signedness belongs to COLUMN_TYPE and DATA_TYPE stays bare,
            // so everything that dispatches on the spelling still sees a
            // `bigint`.
            vec![
                "big",
                "bigint",
                "bigint unsigned",
                "NULL",
                "20",
                "0",
                "NULL"
            ],
            // MySQL 8 deprecated the display width and stopped reporting it.
            vec!["wide", "int", "int", "NULL", "10", "0", "NULL"],
            // Except for a signed TINYINT(1), which is how a boolean is
            // spelled — an unsigned one is not that, and loses the width.
            vec!["flag", "tinyint", "tinyint(1)", "NULL", "3", "0", "NULL"],
            vec![
                "uflag",
                "tinyint",
                "tinyint unsigned",
                "NULL",
                "3",
                "0",
                "NULL"
            ],
            vec![
                "money",
                "decimal",
                "decimal(10,2) unsigned",
                "NULL",
                "10",
                "2",
                "NULL",
            ],
            // A VARBINARY's length is the whole difference between it and a
            // 64KB blob, and it reached CHARACTER_MAXIMUM_LENGTH as 65535.
            vec![
                "vb",
                "varbinary",
                "varbinary(9)",
                "9",
                "NULL",
                "NULL",
                "NULL"
            ],
            vec!["b", "binary", "binary(4)", "4", "NULL", "NULL", "NULL"],
            // Members are quoted, and a quote inside one is doubled.
            vec![
                "pick",
                "set",
                "set('a b','c''d')",
                "7",
                "NULL",
                "NULL",
                "NULL",
            ],
            vec![
                "one",
                "enum",
                "enum('p','q''r')",
                "3",
                "NULL",
                "NULL",
                "NULL"
            ],
            // A DATE carries no fractional seconds at all, where a TIMESTAMP
            // carries zero of them — NULL and 0 are different answers.
            vec!["d", "date", "date", "NULL", "NULL", "NULL", "NULL"],
            vec!["ts", "timestamp", "timestamp", "NULL", "NULL", "NULL", "0"],
            vec!["dt", "datetime", "datetime(3)", "NULL", "NULL", "NULL", "3"],
            // A FLOAT declared with (M,D) reports that M, where a bare one
            // reports the width of its storage.
            vec!["approx", "float", "float(7,3)", "NULL", "7", "3", "NULL"],
            vec![
                "big_approx",
                "double",
                "double unsigned",
                "NULL",
                "22",
                "NULL",
                "NULL",
            ],
        ]
    );
}

#[test]
fn a_numeric_precision_is_reported_the_way_mysql_reports_it() {
    // The `(M)` and `(M,D)` forms are not the same question, and MySQL answers
    // them differently: it fills a DECIMAL's scale in, and reads a lone
    // precision on a FLOAT as a request for mantissa bits — which selects the
    // storage size and then is not reported at all. Every row measured.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Nums (
            bare DECIMAL, p_only DECIMAL(8), both DECIMAL(8,3), num NUMERIC(5),
            f FLOAT, f_small FLOAT(24), f_big FLOAT(25), f_both FLOAT(7,3),
            d DOUBLE, d_both DOUBLE(10,2)
        )",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, NUMERIC_PRECISION, NUMERIC_SCALE \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'Nums' \
             ORDER BY ORDINAL_POSITION"
        ),
        vec![
            // A DECIMAL always reports both, defaulting to MySQL's 10 and 0.
            vec!["bare", "decimal", "decimal(10,0)", "10", "0"],
            vec!["p_only", "decimal", "decimal(8,0)", "8", "0"],
            vec!["both", "decimal", "decimal(8,3)", "8", "3"],
            vec!["num", "decimal", "decimal(5,0)", "5", "0"],
            // A FLOAT reports the width of its storage and no scale.
            vec!["f", "float", "float", "12", "NULL"],
            // `FLOAT(p)` asks for p bits of mantissa; MySQL picks the storage
            // that provides them, reports that type, and drops the p. The
            // manual puts the boundary at 24 and 8.3 promotes at 25.
            vec!["f_small", "float", "float", "12", "NULL"],
            vec!["f_big", "double", "double", "22", "NULL"],
            // Only the two-argument form is echoed, and only it has a scale.
            vec!["f_both", "float", "float(7,3)", "7", "3"],
            vec!["d", "double", "double", "22", "NULL"],
            vec!["d_both", "double", "double(10,2)", "10", "2"],
        ]
    );
}

#[test]
fn a_float_column_holds_and_prints_four_bytes() {
    // A FLOAT is four bytes in MySQL and shows six significant digits. Keeping
    // the double made it a DOUBLE that happened to be declared narrower, and
    // rounding the value without fixing the rendering would have been *worse*
    // — 1.2345678806304932 is further from what MySQL prints than the
    // unrounded value was. Both halves measured on 8.3.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE F (f FLOAT, d DOUBLE)");
    run(
        &engine,
        &mut session,
        "INSERT INTO F VALUES (1.234567890123, 1.234567890123),(0.1,0.1),(1e20,1e20),\
                              (1.5,1.5),(16777217,16777217)",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT f, d FROM F"),
        vec![
            vec!["1.23457", "1.234567890123"],
            vec!["0.1", "0.1"],
            vec!["1e20", "1e20"],
            vec!["1.5", "1.5"],
            // Four bytes cannot hold 16777217, and six digits cannot show
            // 16777216.
            vec!["16777200", "16777217"],
        ]
    );

    // The narrowing does not survive arithmetic: MySQL reads `f + 0` as a
    // double and shows every digit of the widened value.
    assert_eq!(
        rows(&engine, &mut session, "SELECT f + 0 FROM F LIMIT 1"),
        vec![vec!["1.2345678806304932"]]
    );

    // And it says so on the wire, which is what makes the binary protocol send
    // four bytes rather than eight.
    let described = engine
        .describe(&session, "SELECT f, d FROM F", 0)
        .unwrap()
        .unwrap();
    let codes: Vec<String> = described.iter().map(|c| format!("{:?}", c.ty)).collect();
    assert_eq!(codes, vec!["Float", "Double"]);

    // Both renderings, because there are two: `rows` above reads the engine's,
    // and a client reads the protocol's. They were written apart and only one
    // of them knew about the six digits, so the value a test saw and the value
    // a client saw disagreed.
    let Outcome::Rows(result) = run(&engine, &mut session, "SELECT f, d FROM F LIMIT 1") else {
        panic!("expected rows")
    };
    let mut text = mysql_ultralight_protocol::BufWriter::new();
    result.rows[0][0].to_wire().encode_text(&mut text);
    // A length-encoded string: one byte of length, then the digits.
    assert_eq!(
        String::from_utf8_lossy(&text.into_inner()[1..]),
        "1.23457",
        "the protocol renders a FLOAT at six digits too"
    );

    // Four bytes, not eight.
    let mut binary = mysql_ultralight_protocol::BufWriter::new();
    result.rows[0][0]
        .to_wire()
        .encode_binary(&mut binary, described[0].ty);
    assert_eq!(binary.into_inner().len(), 4);
}

#[test]
fn a_float_with_a_scale_is_stored_and_shown_at_that_scale() {
    // `FLOAT(M,D)` and `DOUBLE(M,D)` are not the one-argument form with a
    // display rule bolted on. MySQL *rounds the stored value* to D places on
    // the way in and then shows exactly D of them, always — so 1.234567890123
    // into a `FLOAT(10,4)` reads back `1.2346` and 0.1 reads back `0.1000`.
    // Every value here measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE S (f FLOAT(10,4), d DOUBLE(10,4))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES (1.234567890123, 1.234567890123),(0.1,0.1),(1.5,1.5),\
                              (-3.14159265,-3.14159265),(0,0)",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT f, d FROM S"),
        vec![
            vec!["1.2346", "1.2346"],
            vec!["0.1000", "0.1000"],
            vec!["1.5000", "1.5000"],
            vec!["-3.1416", "-3.1416"],
            vec!["0.0000", "0.0000"],
        ]
    );

    // The rounding is of the *value*, not of its rendering, which is only
    // visible away from the column list: `JSON_ARRAY` reads the number a
    // document holds and ignores the places entirely. `1.2345999479293823` is
    // the four bytes of 1.2346; `1.2345678806304932` would be the four bytes
    // of the value nobody rounded.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(f), JSON_ARRAY(d) FROM S LIMIT 1"
        ),
        vec![vec!["[1.2345999479293823]", "[1.2346]"]]
    );

    // Rounded before the narrowing to four bytes rather than after. 2.00005 is
    // the value that tells the two apart: its `f64` falls just below the half
    // and its `f32` just above, and MySQL stores 2.0000.
    run(&engine, &mut session, "DELETE FROM S");
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES (2.00005, 2.00005)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT f, d FROM S"),
        vec![vec!["2.0000", "2.0000"]]
    );
}

#[test]
fn a_declared_precision_and_scale_are_held_to_mysqls_limits() {
    // Not cosmetic. `M` and `D` arrive as `u64` — the parser takes any number
    // the grammar allows — and a scale is a count of characters written per
    // value: `DECIMAL(10,1000000)` stored 1.5 as a million-character number
    // here, and a fixed-scale float renders the same way, so
    // `DOUBLE(10,4294967295)` is a four-billion-character allocation from one
    // statement. MySQL refuses every one of these at CREATE TABLE.
    //
    // The order is what the codes prove, and it is measured: the scale is
    // checked before the precision, so `FLOAT(300,31)` is 1425 and not 1439
    // and `DECIMAL(66,31)` is 1425 and not 1426. A DECIMAL counts 65 digits
    // and says so with its own code; the approximate pair are measured in
    // display width and stop at 255.
    let (engine, mut session) = engine();
    let scale = "Too big scale 31 specified for column 'x'. Maximum is 30.";
    let m_lt_d = "For float(M,D), double(M,D) or decimal(M,D), M must be >= D (column 'x').";
    let width = "Display width out of range for column 'x' (max = 255)";
    let refused = [
        ("CREATE TABLE B1 (x FLOAT(10,31))", 1425, scale),
        ("CREATE TABLE B2 (x DOUBLE(10,31))", 1425, scale),
        ("CREATE TABLE B3 (x DECIMAL(10,31))", 1425, scale),
        (
            "CREATE TABLE B4 (x DECIMAL(10,1000000))",
            1425,
            "Too big scale 1000000 specified for column 'x'. Maximum is 30.",
        ),
        ("CREATE TABLE B5 (x FLOAT(300,31))", 1425, scale),
        ("CREATE TABLE B6 (x DECIMAL(66,31))", 1425, scale),
        ("CREATE TABLE B7 (x FLOAT(300,2))", 1439, width),
        ("CREATE TABLE B8 (x DOUBLE(256,2))", 1439, width),
        (
            "CREATE TABLE B9 (x DECIMAL(66,2))",
            1426,
            "Too-big precision 66 specified for 'x'. Maximum is 65.",
        ),
        ("CREATE TABLE Ba (x FLOAT(2,5))", 1427, m_lt_d),
        ("CREATE TABLE Bb (x DECIMAL(2,5))", 1427, m_lt_d),
    ];
    for (sql, code, message) in refused {
        let error = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(error.code, code, "{sql} -> {}", error.message);
        // The words too, not only the code: they carry the limit, and a client
        // that prints the error is what a migration author reads.
        assert_eq!(error.message, message, "{sql}");
    }

    // Every path that builds a column, not just `CREATE TABLE`: an ALTER
    // that adds or redefines one declares a type in exactly the same words.
    run(&engine, &mut session, "CREATE TABLE Bz (x INT)");
    for sql in [
        "ALTER TABLE Bz ADD COLUMN y DECIMAL(10,31)",
        "ALTER TABLE Bz MODIFY COLUMN x DOUBLE(10,31)",
        "ALTER TABLE Bz CHANGE COLUMN x w FLOAT(10,31)",
    ] {
        let error = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(error.code, 1425, "{sql} -> {}", error.message);
    }

    // The widest each type really allows still goes through, and so does a
    // lone precision — `DECIMAL(8)` is `DECIMAL(8,0)`, not an error.
    for sql in [
        "CREATE TABLE Bc (x FLOAT(255,30))",
        "CREATE TABLE Bd (x DOUBLE(255,30))",
        "CREATE TABLE Be (x DECIMAL(65,30))",
        "CREATE TABLE Bf (x DECIMAL(8))",
        "CREATE TABLE Bg (x FLOAT(25))",
    ] {
        run(&engine, &mut session, sql);
    }
}

#[test]
fn a_wide_scale_shows_the_shortest_digits_that_read_back() {
    // The places are padded onto the *shortest* string that round-trips, not
    // onto the double's binary expansion, and past about seventeen significant
    // digits those are different numbers. Both rows measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE P (a DOUBLE(20,17), b DOUBLE(255,30), c FLOAT(40,2))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO P VALUES (0.1, 1.5, 1e30)",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT a, b, c FROM P"),
        vec![vec![
            // The expansion is 0.10000000000000001.
            "0.10000000000000000",
            // The expansion is 1.500000000000000222044604925031.
            "1.500000000000000000000000000000",
            // Four bytes cannot hold 1e30, and the digits that read those four
            // bytes back stop well before the point.
            "1000000015047466200000000000000.00",
        ]]
    );

    // And the value behind them is the one four or eight bytes hold: the wide
    // scale must not have moved it. Dividing back by a power of ten would
    // have, because neither power is exact — 1.5 came back
    // 1.5000000000000002.
    assert_eq!(
        rows(&engine, &mut session, "SELECT JSON_ARRAY(b) FROM P"),
        vec![vec!["[1.5]"]]
    );
}

#[test]
fn a_value_that_rounds_to_zero_loses_its_sign() {
    // -0.001 into a `FLOAT(10,2)` rounds to a negative zero, and MySQL stores
    // the positive one: 8.3 answers `0.00`, `[0.0]` and a plain zero on the
    // wire. The sign survives only where the rounding does not reach zero,
    // which is why -0.006 is here beside it.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Zs (id INT, f FLOAT(10,2), d DOUBLE(10,2))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Zs VALUES (1,-0.001,-0.001),(2,-0.004,-0.004),(3,-0.006,-0.006)",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT f, d FROM Zs ORDER BY id"),
        vec![
            vec!["0.00", "0.00"],
            vec!["0.00", "0.00"],
            vec!["-0.01", "-0.01"],
        ]
    );
    // Not a rendering rule: the sign is gone from the stored number too, so a
    // document and a binary client see it gone as well.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(f, d) FROM Zs WHERE id = 1"
        ),
        vec![vec!["[0.0, 0.0]"]]
    );
}

#[test]
fn a_cast_to_a_decimal_is_held_to_mysqls_own_limits() {
    // The same one-statement blow-up the column check closes, reachable by
    // anyone who can send a SELECT: `CAST(1.5 AS DECIMAL(10,1000000))`
    // answered a million-digit number here.
    //
    // A cast's rule is not the column's, and neither is its order — measured
    // on 8.3, it checks the precision, then M >= D, and only then the scale.
    // `DECIMAL(70,40)` is 1426 where the column form is 1425, and
    // `DECIMAL(10,31)` is 1427 where the column form is 1425.
    let (engine, mut session) = engine();
    let m_lt_d = "For float(M,D), double(M,D) or decimal(M,D), M must be >= D (column '').";
    let refused = [
        ("SELECT CAST(1.5 AS DECIMAL(10,31))", 1427, m_lt_d),
        ("SELECT CAST(1.5 AS DECIMAL(2,5))", 1427, m_lt_d),
        ("SELECT CAST(1.5 AS DECIMAL(10,1000000))", 1427, m_lt_d),
        (
            "SELECT CAST(1.5 AS DECIMAL(66,2))",
            1426,
            "Too-big precision 66 specified for '1.5'. Maximum is 65.",
        ),
        (
            "SELECT CAST(1.5 AS DECIMAL(70,40))",
            1426,
            "Too-big precision 70 specified for '1.5'. Maximum is 65.",
        ),
        (
            "SELECT CAST(1.5 AS DECIMAL(65,31))",
            1425,
            "Too big scale 31 specified for column '1.5'. Maximum is 30.",
        ),
    ];
    for (sql, code, message) in refused {
        let error = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(error.code, code, "{sql} -> {}", error.message);
        assert_eq!(error.message, message, "{sql}");
    }

    // The widest a cast really allows still converts.
    assert_eq!(
        rows(&engine, &mut session, "SELECT CAST(1.5 AS DECIMAL(65,30))"),
        vec![vec!["1.500000000000000000000000000000"]]
    );
}

#[test]
fn a_set_operation_carries_the_wider_scale_and_rounds_into_it() {
    // A set operation materialises its result, and MySQL's temporary table has
    // one field per column: both branches are written into it and read back at
    // *its* scale, which is the wider of the two. Measured on 8.3, including
    // the width — `FLOAT(12,2) UNION ALL FLOAT(10,4)` is `float(14,4)` read in
    // either order.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Su (f FLOAT(10,4), d DOUBLE(10,4), a FLOAT(12,2), h DOUBLE(12,6), pd DOUBLE)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Su VALUES (0.1, 0.1, 0.1, 0.1, 0.1)",
    );

    let cases = [
        // The widening is where the scale used to be dropped: these three
        // change the type code, and only a change of code cleared it.
        ("SELECT f FROM Su UNION ALL SELECT d FROM Su", "0.1000"),
        ("SELECT f FROM Su UNION ALL SELECT h FROM Su", "0.100000"),
        // And the same code with different scales, which takes the other
        // branch: the wider *declaration* is not the wider scale, and reading
        // the pair in this order used to answer two places.
        ("SELECT f FROM Su UNION ALL SELECT a FROM Su", "0.1000"),
        ("SELECT a FROM Su UNION ALL SELECT f FROM Su", "0.1000"),
    ];
    for (sql, expected) in cases {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected], vec![expected]],
            "{sql}"
        );
    }

    // A plain DOUBLE has no scale to merge, so the column has none either and
    // every digit of the widened four-byte value shows, exactly as MySQL does.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT f FROM Su UNION ALL SELECT pd FROM Su"
        ),
        vec![vec!["0.10000000149011612"], vec!["0.1"]]
    );

    // One row, as MySQL answers: both branches are written into the same
    // field and read back as the same number.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT f FROM Su UNION SELECT d FROM Su"
        ),
        vec![vec!["0.1000"]]
    );

    // The rounding is of the *value*, not of the printing, and the difference
    // only shows once the result feeds another query — a rendering at the
    // merged scale would print `0.1000` over whatever number was behind it.
    // Read through a document, which takes the number and ignores the places:
    // the four-byte 0.1 widens to 0.10000000149011612 and has to become the
    // eight-byte 0.1 that the other branch already holds.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(x) FROM \
             (SELECT f AS x FROM Su UNION ALL SELECT d FROM Su) t ORDER BY 1"
        ),
        vec![vec!["[0.1]"], vec!["[0.1]"]]
    );
    // And they really are one number afterwards, which is the same claim from
    // the other side.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(DISTINCT x) FROM \
             (SELECT f AS x FROM Su UNION ALL SELECT d FROM Su) t"
        ),
        vec![vec!["1"]]
    );
}

#[test]
fn a_view_hands_its_rows_on_without_shaping_them_for_its_own_columns() {
    // A view runs a whole query, and its result is not on its way out — it is
    // about to feed another one. Reconciling it there shaped every value for
    // the *inner* column's code, and the outer query then read it under a code
    // of its own: a `double(10,4)` came back rendered at its four places, was
    // converted back to a plain double by the DOUBLE arm on the second pass,
    // and lost them, while the `float(10,4)` beside it kept them. One rule,
    // two answers, decided by which arm the second pass happened to have.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Vt (f FLOAT(10,4), d DOUBLE(10,4))",
    );
    run(&engine, &mut session, "INSERT INTO Vt VALUES (0.1, 0.1)");
    run(
        &engine,
        &mut session,
        "CREATE VIEW Vv AS SELECT f, d FROM Vt",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT f, d FROM Vv"),
        vec![vec!["0.1000", "0.1000"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT * FROM Vv"),
        vec![vec!["0.1000", "0.1000"]]
    );
}

#[test]
fn a_float_with_a_scale_rounds_a_tie_to_the_even_digit() {
    // `rint`'s rule and not `round`'s. These three are exact halves after the
    // multiply, so nothing but the tie rule decides them; 0.135 and 0.145
    // beside them are not halves at all — their doubles fall just above and
    // just below — and they answer 0.14 either way, which is why they are no
    // evidence and these are.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE T (f FLOAT(10,2))");
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES (0.125),(0.015),(0.025),(-0.125)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT f FROM T"),
        vec![vec!["0.12"], vec!["0.02"], vec!["0.02"], vec!["-0.12"],]
    );

    // Both renderings, for the reason the test above reads both: a client
    // reads the protocol's and a test reads the engine's, and a fix applied to
    // one of them only is invisible to the other.
    let Outcome::Rows(result) = run(&engine, &mut session, "SELECT f FROM T LIMIT 1") else {
        panic!("expected rows")
    };
    let mut text = mysql_ultralight_protocol::BufWriter::new();
    result.rows[0][0].to_wire().encode_text(&mut text);
    assert_eq!(
        String::from_utf8_lossy(&text.into_inner()[1..]),
        "0.12",
        "the protocol shows the declared places too"
    );

    // And the binary protocol still sends the column's four bytes, which is
    // what the type code promises: the places are a rendering, not a change of
    // type. MySQL announces MYSQL_TYPE_FLOAT for a `FLOAT(M,D)` and sends four
    // bytes of the rounded value.
    let described = engine
        .describe(&session, "SELECT f FROM T", 0)
        .unwrap()
        .unwrap();
    assert_eq!(format!("{:?}", described[0].ty), "Float");
    let mut binary = mysql_ultralight_protocol::BufWriter::new();
    result.rows[0][0]
        .to_wire()
        .encode_binary(&mut binary, described[0].ty);
    assert_eq!(binary.into_inner(), 0.12f32.to_le_bytes());
}

#[test]
fn a_float_wider_than_its_declared_digits_is_refused() {
    // `FLOAT(M,D)` holds M digits with D after the point, so a `FLOAT(6,2)`
    // stops below 10000. MySQL raises 1264 rather than truncating, and it does
    // so *after* rounding: 9999.994 goes in and 9999.995 does not, because the
    // second rounds to 10000.00. Both measured on 8.3, and a `DOUBLE(6,2)`
    // beside it answers the same way.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE W (f FLOAT(6,2), d DOUBLE(6,2))",
    );

    for sql in [
        "INSERT INTO W (f) VALUES (12345.678)",
        "INSERT INTO W (f) VALUES (9999.995)",
        "INSERT INTO W (d) VALUES (12345.678)",
        "INSERT INTO W (d) VALUES (-9999.995)",
    ] {
        let error = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(error.code, 1264, "{sql}");
    }

    run(
        &engine,
        &mut session,
        "INSERT INTO W VALUES (9999.994, 9999.994)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT f, d FROM W"),
        vec![vec!["9999.99", "9999.99"]]
    );
}

#[test]
fn a_float_expression_is_typed_the_way_its_value_is_tagged() {
    // Whether a FLOAT survives an expression is decided per function, and
    // MySQL is not uniform about it: `-f`, `ABS(f)` and `ROUND(f, 2)` widen to
    // a double and show every digit, while `COALESCE(f, 0)`, `LEAST(f, 9)` and
    // a cast back to FLOAT stay six digits wide. All measured on 8.3.
    //
    // The type and the value are decided in different code — the type in
    // `computed_column_type`, the tag in the evaluator — so this reads both
    // for every expression. Where they disagree the two protocols answer
    // differently for the same row, which is the bug that is worth a test:
    // the text one prints seventeen digits and the binary one writes four
    // bytes of something else.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE F (f FLOAT)");
    run(
        &engine,
        &mut session,
        "INSERT INTO F VALUES (1.234567890123)",
    );

    let cases = [
        ("-f", "Double", "-1.2345678806304932"),
        ("ABS(f)", "Double", "1.2345678806304932"),
        ("ROUND(f, 2)", "Double", "1.23"),
        ("f + 0", "Double", "1.2345678806304932"),
        ("COALESCE(f, 0)", "Float", "1.23457"),
        ("LEAST(f, 9)", "Float", "1.23457"),
        ("GREATEST(f, 0)", "Float", "1.23457"),
        ("CAST(f AS FLOAT)", "Float", "1.23457"),
    ];
    for (expression, expected_type, expected_text) in cases {
        let sql = format!("SELECT {expression} AS v FROM F");
        let described = engine.describe(&session, &sql, 0).unwrap().unwrap();
        assert_eq!(
            format!("{:?}", described[0].ty),
            expected_type,
            "{expression} is described as the wrong type"
        );
        assert_eq!(
            rows(&engine, &mut session, &sql),
            vec![vec![expected_text]],
            "{expression} answers the wrong value"
        );
        // The declared code decides how many bytes the binary protocol writes,
        // so a value tagged the other way sends a different number entirely.
        let Outcome::Rows(result) = run(&engine, &mut session, &sql) else {
            panic!("expected rows")
        };
        let mut binary = mysql_ultralight_protocol::BufWriter::new();
        result.rows[0][0]
            .to_wire()
            .encode_binary(&mut binary, described[0].ty);
        let expected_width = if expected_type == "Float" { 4 } else { 8 };
        assert_eq!(
            binary.into_inner().len(),
            expected_width,
            "{expression} encodes to the wrong width"
        );
    }
}

#[test]
fn a_float_converts_to_a_decimal_by_its_value_and_not_by_its_display() {
    // The six significant digits are a *rendering*, and converting out of them
    // makes the answer depend on how wide the column happened to print:
    // `CAST(f AS DECIMAL(20,10))` over a FLOAT holding 1.234567890123 is
    // 1.2345678806 on 8.3 and was 1.2345700000 here, read back out of
    // `1.23457`. #0071 already made this argument for `CAST(f AS JSON)`, which
    // is the same value asked for through a different conversion — and the two
    // disagreeing about one number is worse than either being wrong.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE C (f FLOAT, d DOUBLE)");
    run(
        &engine,
        &mut session,
        "INSERT INTO C VALUES (1.234567890123, 1.234567890123),(16777217, 16777217)",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST(f AS DECIMAL(20,10)), CAST(d AS DECIMAL(20,10)) FROM C"
        ),
        vec![
            vec!["1.2345678806", "1.2345678901"],
            // Four bytes cannot hold 16777217, and the conversion says so
            // where the display's `16777200` did not.
            vec!["16777216.0000000000", "16777217.0000000000"],
        ]
    );
}

#[test]
fn a_float_that_four_bytes_cannot_hold_is_refused() {
    // `as f32` saturates rather than failing, so the column stored an infinity
    // and answered the token `Inf` — and kept it, because the image writes the
    // bits. MySQL refuses the statement: 1264, the same code the unsigned
    // guard beside it uses.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE F (f FLOAT)");

    for sql in [
        "INSERT INTO F VALUES (3.5e38)",
        "INSERT INTO F VALUES (-3.5e38)",
    ] {
        let error = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(error.code, 1264, "{sql}");
    }

    run(&engine, &mut session, "INSERT INTO F VALUES (1)");
    let error = engine
        .execute(&mut session, "UPDATE F SET f = 1e300", &[])
        .unwrap_err();
    assert_eq!(error.code, 1264);

    // A cast is refused too, and MySQL uses a different code for it because
    // there is no column to name.
    let error = engine
        .execute(&mut session, "SELECT CAST(1e300 AS FLOAT)", &[])
        .unwrap_err();
    assert_eq!(error.code, 1690);
    assert!(
        error.message.contains("cast(1e300 as float)"),
        "the operand is named the way MySQL names it: {}",
        error.message
    );

    // What four bytes *can* hold still goes in.
    run(&engine, &mut session, "UPDATE F SET f = 3.4e38");
    assert_eq!(
        rows(&engine, &mut session, "SELECT f FROM F"),
        vec![vec!["3.4e38"]]
    );
}

#[test]
fn a_decimal_column_never_answers_in_exponent_notation() {
    // All non-integer arithmetic evaluates to a float while the column is
    // declared NEWDECIMAL, so a sum over a wide decimal reached the float
    // formatter and came back `4e16` once that formatter learned about
    // exponents. MySQL never sends a NEWDECIMAL that way, and a driver handing
    // the text to a decimal parser has no reason to accept it.
    //
    // The scale is still the f64's rather than the one MySQL computes — that
    // is #0070, and this only pins the form.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE D (amt DECIMAL(30,2))");
    run(
        &engine,
        &mut session,
        "INSERT INTO D VALUES (20000000000000000.00)",
    );

    for row in rows(&engine, &mut session, "SELECT amt + amt, amt * 2 FROM D") {
        for value in row {
            assert!(
                !value.contains('e') && !value.contains('E'),
                "a decimal column answered `{value}`"
            );
        }
    }
}

#[test]
fn a_double_is_printed_the_way_mysql_prints_one() {
    // Rust's `Display` never reaches for an exponent, so `1e20` was written as
    // twenty-one digits and DBL_MAX as three hundred and nine. The rule is
    // `my_gcvt`'s, the same one a document's numbers go through — and a column
    // drops the trailing `.0` a document keeps.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE D2 (n INT, d DOUBLE)");
    run(
        &engine,
        &mut session,
        "INSERT INTO D2 VALUES (14,1e14),(15,1e15),(16,1e16),(20,1e20),(-15,1e-15),\
                               (-16,1e-16),(1,0.1),(2,1.5),(3,-2.0),(4,1.234567890123),\
                               (5,123456789012345678),(6,1e308)",
    );

    assert_eq!(
        rows(&engine, &mut session, "SELECT d FROM D2 ORDER BY n")
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        vec![
            "1e-16",
            "0.000000000000001",
            "0.1",
            "1.5",
            // A column drops the `.0`; a document would keep it.
            "-2",
            "1.234567890123",
            // Seventeen digits do not fit in eighteen places, so this one
            // takes the exponent even though the digits reach past the point.
            "1.2345678901234568e17",
            "1e308",
            "100000000000000",
            "1e15",
            "1e16",
            "1e20",
        ]
    );
}

#[test]
fn soundex_answers_what_mysql_answers() {
    // The classic algorithm, with three of its own answers to the places the
    // variants disagree — each measured on 8.3 rather than taken from a
    // description of Soundex.
    let (engine, mut session) = engine();

    for (sql, expected) in [
        ("SELECT SOUNDEX('Robert')", "R163"),
        ("SELECT SOUNDEX('Rupert')", "R163"),
        // Not truncated to four. This is where most descriptions stop.
        ("SELECT SOUNDEX('Ashcraft')", "A2613"),
        ("SELECT SOUNDEX('Washington')", "W25235"),
        // A vowel does not break a run: the K still reads as a repeat of the
        // Z across the A between them, so this is T520 and not T522.
        ("SELECT SOUNDEX('Tymczak')", "T520"),
        // The first letter seeds that run, so the F is swallowed by the P.
        ("SELECT SOUNDEX('Pfister')", "P236"),
        ("SELECT SOUNDEX('Honeyman')", "H500"),
        ("SELECT SOUNDEX('Smith')", "S530"),
        ("SELECT SOUNDEX('Smythe')", "S530"),
        ("SELECT SOUNDEX('Tomorrow')", "T560"),
        ("SELECT SOUNDEX('Lukasiewicz')", "L200"),
        ("SELECT SOUNDEX('bcdl')", "B234"),
        // H, W and Y behave like vowels — none of these gets past its B.
        ("SELECT SOUNDEX('Bhbb')", "B000"),
        ("SELECT SOUNDEX('Bwb')", "B000"),
        // Case is folded and everything that is not a letter is skipped,
        // including before the first one.
        ("SELECT SOUNDEX('robert')", "R163"),
        ("SELECT SOUNDEX('  $$ Robert 99')", "R163"),
        ("SELECT SOUNDEX('9abc')", "A120"),
        // A letter outside ASCII counts as a letter and codes as zero.
        ("SELECT SOUNDEX('café')", "C100"),
        // Padded only when short, and the first letter stands even when its
        // own code is zero.
        ("SELECT SOUNDEX('a')", "A000"),
        ("SELECT SOUNDEX('h')", "H000"),
        // Nothing to work with gives the empty string, not NULL.
        ("SELECT SOUNDEX('')", ""),
        ("SELECT SOUNDEX('   ')", ""),
        ("SELECT SOUNDEX('123')", ""),
        // NULL is the one thing that gives NULL.
        ("SELECT SOUNDEX(NULL)", "NULL"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // The letter test past ASCII, all measured on 8.3 through a utf8mb4
    // connection. It is a range over the codepoint rather than a Unicode
    // property — ASCII, or U+00C0 and above — so `µ` and `ª` are alphabetic to
    // Rust and not to MySQL, and `×` is the reverse. The character that passes
    // is emitted whole and unfolded, and codes as zero.
    //
    // #0074 recorded these as unreachable, on the strength of `SOUNDEX('€abc')`
    // measuring as the single byte 0xE2 followed by `120`. That reading came
    // through a *latin1* connection, which gives MySQL each UTF-8 byte as its
    // own latin1 character — and a lead byte is 0xC0 or above, so it passes
    // this same test while the continuation bytes fail it.
    for (sql, expected) in [
        ("SELECT SOUNDEX('éabc')", "é120"),
        ("SELECT SOUNDEX('€abc')", "€120"),
        ("SELECT SOUNDEX('🙂abc')", "🙂120"),
        // Below U+00C0, so not a letter: the `a` becomes the first character.
        ("SELECT SOUNDEX('µabc')", "A120"),
        ("SELECT SOUNDEX('ªabc')", "A120"),
        // Above it, so a letter, though Rust would not call it one.
        ("SELECT SOUNDEX('×abc')", "×120"),
        // Position matters only for what lands first. Everywhere else a
        // non-ASCII letter codes zero, like a vowel.
        ("SELECT SOUNDEX('abc€')", "A120"),
        ("SELECT SOUNDEX('aé')", "A000"),
        ("SELECT SOUNDEX('b€b')", "B000"),
        // A combining mark is a character like any other, and this one is
        // below U+00C0, so `e` stands alone as the first letter.
        ("SELECT SOUNDEX('e\u{301}abc')", "E120"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // A *binary* string is a different letter test, and the same measurements
    // through the same connection. A single-byte charset uses its own ctype
    // table instead of the codepoint range above, and the binary charset
    // classifies only ASCII — of all 256 byte values exactly `A`-`Z` and
    // `a`-`z` are letters on 8.3 — so every byte above 0x7F is skipped as
    // though it were punctuation.
    for (sql, expected) in [
        ("SELECT SOUNDEX(CAST('éabc' AS BINARY))", "A120"),
        ("SELECT SOUNDEX(BINARY 'éabc')", "A120"),
        ("SELECT SOUNDEX(X'C3A9616263')", "A120"),
        // Nothing but high bytes leaves no letter at all, where the same
        // characters as text answer `é000`.
        ("SELECT SOUNDEX(CAST('éé' AS BINARY))", ""),
        // ASCII reads identically through both, which is what makes the
        // divergence a leading-character one and never a digit one: a high
        // byte codes zero on one path and is absent on the other, and neither
        // writes a digit.
        ("SELECT SOUNDEX(CAST('Robert' AS BINARY))", "R163"),
        ("SELECT SOUNDEX(CAST('Ashcraft' AS BINARY))", "A2613"),
        ("SELECT SOUNDEX(CAST('café' AS BINARY))", "C100"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn a_json_double_is_written_the_way_mysql_writes_one() {
    // Three formatters disagree here and only one is MySQL's. serde switches
    // to exponent form earlier than MySQL and writes the sign — `1e+30` where
    // MySQL writes `1e30`. Rust's own `Display` never switches at all, so
    // `1e30` comes out as thirty-one digits. Every row measured on 8.3.
    let (engine, mut session) = engine();

    for (sql, expected) in [
        // Fixed notation up to an exponent of 14, and exponent form from 15.
        (
            "SELECT CAST('[1e13, 1e14, 1e15, 1e16, 1e30, 1e308]' AS JSON)",
            "[10000000000000.0, 100000000000000.0, 1e15, 1e16, 1e30, 1e308]",
        ),
        // And down to -15, with -16 the first to switch.
        (
            "SELECT CAST('[1e-1, 1e-7, 1e-15, 1e-16, 1e-308]' AS JSON)",
            "[0.1, 0.0000001, 0.000000000000001, 1e-16, 1e-308]",
        ),
        // A whole double keeps its `.0` inside the window — that is what makes
        // it a double rather than the integer beside it.
        (
            "SELECT CAST('[2.0, 2.5, -2.0, 0.0, -0.0]' AS JSON)",
            "[2.0, 2.5, -2.0, 0.0, -0.0]",
        ),
        // An integer is not a double and gets none of this.
        ("SELECT CAST('[1, 2, -3, 0]' AS JSON)", "[1, 2, -3, 0]"),
        // The mantissa is carried across the threshold, not just the power.
        (
            "SELECT CAST('[1.5e14, 1.5e15, 1.23456789012345e20]' AS JSON)",
            "[150000000000000.0, 1.5e15, 1.23456789012345e20]",
        ),
        // Above the threshold the exponent is not the whole test: a value
        // whose digits reach past the point stays written out, because they
        // do not fit in the places before it. `1e15` is exponential and
        // `1000000000000000.5` is not, at the same exponent.
        (
            "SELECT CAST('[1000000000000000.5, 4000000000000000.5, 2000000000000000.3]' AS JSON)",
            "[1000000000000000.4, 4.000000000000001e15, 2000000000000000.5]",
        ),
        // Built rather than parsed, so the rule is in the writer and not in
        // whatever the source text happened to say.
        (
            "SELECT JSON_OBJECT('a', 1e15, 'b', 1e14, 'c', 2.0)",
            r#"{"a": 1e15, "b": 100000000000000.0, "c": 2.0}"#,
        ),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn a_decimal_in_a_document_keeps_its_scale() {
    // The scale is the answer: a client reading a money column out of a
    // document cares that 2.50 came back and not 2.5. Every row measured on
    // 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE M (id INT NOT NULL, a DECIMAL(10,2) NULL, b DECIMAL(10,4) NULL, \
                         c DECIMAL(10,0) NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO M VALUES (1, '2.50', '2.5000', '2'), (2, '-3.10', '-3.1000', '-3'), \
                              (3, '0.00', '0.0000', '0')",
    );

    for (sql, expected) in [
        // Every constructor, because each one builds the document its own way
        // and only the first was ever looked at.
        (
            "SELECT JSON_OBJECT('a', a, 'b', b, 'c', c) FROM M WHERE id = 1",
            r#"{"a": 2.50, "b": 2.5000, "c": 2}"#,
        ),
        (
            "SELECT JSON_ARRAY(a, b, c) FROM M WHERE id = 2",
            "[-3.10, -3.1000, -3]",
        ),
        // Zero is written to its scale like anything else — `0.00`, not `0`.
        (
            "SELECT JSON_ARRAY(a, b, c) FROM M WHERE id = 3",
            "[0.00, 0.0000, 0]",
        ),
        // A cast converts to the JSON decimal rather than reading the text
        // back, which is the difference between 2.50 and the double 2.5.
        ("SELECT CAST(a AS JSON) FROM M WHERE id = 1", "2.50"),
        (
            "SELECT JSON_SET('{\"x\": 1}', '$.x', a) FROM M WHERE id = 1",
            r#"{"x": 2.50}"#,
        ),
        (
            "SELECT JSON_MERGE_PATCH(JSON_OBJECT('a', a), '{\"b\": 2}') FROM M WHERE id = 1",
            r#"{"a": 2.50, "b": 2}"#,
        ),
        ("SELECT JSON_ARRAYAGG(a) FROM M", "[2.50, -3.10, 0.00]"),
        // An aggregate's own decimal, not only a column's.
        ("SELECT JSON_ARRAY(SUM(a)) FROM M", "[-0.60]"),
        // Extraction gives back what was put in, both ways.
        (
            "SELECT JSON_EXTRACT(JSON_OBJECT('a', a), '$.a') FROM M WHERE id = 1",
            "2.50",
        ),
        (
            "SELECT JSON_UNQUOTE(JSON_EXTRACT(JSON_OBJECT('a', a), '$.a')) FROM M WHERE id = 1",
            "2.50",
        ),
        // Source text is the other side of the boundary and has no decimal in
        // it: MySQL's parser reads `2.50` as the double 2.5, `-0` as the
        // integer 0, and `1E2` as `100.0`.
        ("SELECT CAST('[2.50, -0, 1E2]' AS JSON)", "[2.5, 0, 100.0]"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // Through storage and back. A JSON column holds the document a
    // constructor built, so the scale survives being written and read; the
    // same digits written as *text* are the source-text parse and do not.
    run(&engine, &mut session, "CREATE TABLE MJ (id INT, doc JSON)");
    run(
        &engine,
        &mut session,
        "INSERT INTO MJ SELECT id, JSON_OBJECT('a', a) FROM M WHERE id = 1",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO MJ VALUES (9, '{\"a\": 2.50}')",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT doc, doc->'$.a', doc->>'$.a' FROM MJ ORDER BY id"
        ),
        vec![
            vec![r#"{"a": 2.50}"#, "2.50", "2.50"],
            vec![r#"{"a": 2.5}"#, "2.5", "2.5"],
        ]
    );
}

#[test]
fn a_document_s_numbers_compare_by_value_and_exactly() {
    // Two questions in one, because the answers have to agree: `2.50`, `2.5`
    // and `2` are one number, and two decimals a double cannot tell apart are
    // two. Measured on 8.3 — it answers 1 for the first pair and 0 for the
    // second, and counts two groups either way.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE C (id INT NOT NULL, a DECIMAL(10,2) NULL, w DECIMAL(30,2) NULL, \
                         PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO C VALUES (1, '2.50', '123456789012345678901.00'), \
                              (2, '2.00', '123456789012345678902.00')",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(a) = CAST('[2.5]' AS JSON), \
                    JSON_ARRAY(a) = CAST('[2.50]' AS JSON) FROM C WHERE id = 1"
        ),
        vec![vec!["1", "1"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(a) = CAST('[2]' AS JSON) FROM C WHERE id = 2"
        ),
        vec![vec!["1"]]
    );
    // Past what a double can separate. Through one, these two documents were
    // equal and DISTINCT kept one row.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT (SELECT JSON_ARRAY(w) FROM C WHERE id = 1) \
                  = (SELECT JSON_ARRAY(w) FROM C WHERE id = 2)"
        ),
        vec![vec!["0"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(DISTINCT JSON_ARRAY(w)) FROM C"
        ),
        vec![vec!["2"]]
    );
    // And the key agrees with the comparison: one group for the spellings the
    // comparison calls equal.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(DISTINCT CAST(v AS JSON)) FROM \
             (SELECT '2.50' v UNION ALL SELECT '2.5' UNION ALL SELECT '2.500') x"
        ),
        vec![vec!["1"]]
    );
    // JSON_CONTAINS asks the same question and has to give the same answer:
    // `[2.0]` contains `2` on 8.3.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_CONTAINS(JSON_OBJECT('a', a), '2.5', '$.a'), \
                    JSON_CONTAINS('[2.0]', '2'), JSON_CONTAINS('[2]', '2.0') \
             FROM C WHERE id = 1"
        ),
        vec![vec!["1", "1", "1"]]
    );
}

#[test]
fn the_number_sentinel_key_is_an_ordinary_key() {
    // `$serde_json::private::Number` is how serde_json passes a number it is
    // holding as text through a visitor, and its own reader turns any object
    // with that key back into a number. MySQL has never heard of it: measured
    // on 8.3, every one of these is an object, `JSON_VALID` is 1 for all
    // three spellings, and the key comes back out of an extract.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE S (id INT NOT NULL, doc JSON NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES \
         (1, '{\"$serde_json::private::Number\": \"1\"}'), \
         (2, '{\"$serde_json::private::Number\": 1}'), \
         (3, '{\"$serde_json::private::Number\": \"abc\"}'), \
         (5, '{\"$serde_json::private::Number\": \"1\", \"b\": 2}'), \
         (6, '{\"b\": 2, \"$serde_json::private::Number\": \"1\"}'), \
         (7, '[2.50, {\"$serde_json::private::Number\": \"1\"}]')",
    );
    // Built by this engine's own constructor, not only written as text. This
    // is the one that used to fail 3140 against the engine's own rendering.
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES (4, JSON_OBJECT('$serde_json::private::Number', 1))",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, doc, JSON_TYPE(doc), JSON_LENGTH(doc) FROM S ORDER BY id"
        ),
        vec![
            vec![
                "1",
                r#"{"$serde_json::private::Number": "1"}"#,
                "OBJECT",
                "1"
            ],
            vec!["2", r#"{"$serde_json::private::Number": 1}"#, "OBJECT", "1"],
            vec![
                "3",
                r#"{"$serde_json::private::Number": "abc"}"#,
                "OBJECT",
                "1"
            ],
            vec!["4", r#"{"$serde_json::private::Number": 1}"#, "OBJECT", "1"],
            vec![
                "5",
                r#"{"b": 2, "$serde_json::private::Number": "1"}"#,
                "OBJECT",
                "2"
            ],
            vec![
                "6",
                r#"{"b": 2, "$serde_json::private::Number": "1"}"#,
                "OBJECT",
                "2"
            ],
            // The array is the pair that matters most: one document holding
            // both a real number and that key, which have to come out as two
            // different things.
            vec![
                "7",
                r#"[2.5, {"$serde_json::private::Number": "1"}]"#,
                "ARRAY",
                "2"
            ],
        ]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_VALID('{\"$serde_json::private::Number\": \"1\"}'), \
                    JSON_VALID('{\"$serde_json::private::Number\": 1}'), \
                    JSON_VALID('{\"$serde_json::private::Number\": \"abc\"}')"
        ),
        vec![vec!["1", "1", "1"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_EXTRACT(doc, '$.\"$serde_json::private::Number\"') FROM S WHERE id = 1"
        ),
        vec![vec![r#""1""#]]
    );
}

#[test]
fn a_cast_to_json_converts_an_approximate_number_rather_than_reading_its_text() {
    // A FLOAT displays six significant digits, so reading a cast back from
    // the column's own text gave `1.23457` where 8.3 answers the widened
    // double. Both routes into a document have to agree, and only one of
    // them was MySQL's. Measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE F (f FLOAT NULL, d DOUBLE NULL)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO F VALUES (1.234567890123, 1.234567890123), (16777217, 16777217)",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST(f AS JSON), JSON_ARRAY(f), CAST(d AS JSON), JSON_ARRAY(d) \
             FROM F ORDER BY f"
        ),
        vec![
            vec![
                "1.2345678806304932",
                "[1.2345678806304932]",
                "1.234567890123",
                "[1.234567890123]"
            ],
            vec!["16777216.0", "[16777216.0]", "16777217.0", "[16777217.0]"],
        ]
    );
}

#[test]
fn a_json_number_is_read_the_way_mysqls_parser_reads_one() {
    // Which double a literal names is as much MySQL's answer as how it is
    // written back, and the two parsers already in reach give a different one.
    // MySQL reads JSON with rapidjson at its default precision, which keeps
    // seventeen significant digits and scales them by one tabulated power of
    // ten; Rust's own parse is correctly rounded and serde's keeps nineteen.
    // #0072. Every row measured on 8.3.
    let (engine, mut session) = engine();

    for (sql, expected) in [
        // The one the issue was written about, and its mirror.
        // `2000000000000000.25` names a double exactly, and every one of its
        // digits is read — it still is not that double, because the
        // accumulator ties to 2e16 on its way into one, the seventeenth digit
        // ties again on its way in after that, and dividing by a hundred
        // lands on 2e15.
        (
            "SELECT CAST('[2000000000000000.25, -2000000000000000.25]' AS JSON)",
            "[2e15, -2e15]",
        ),
        // Zeros spend the budget of seventeen like any other digit, so the
        // padded spelling of a number is a different number: two of these
        // zeros are read and thrown away, and one of these is a digit short.
        (
            "SELECT CAST('[6193038.5328500000000, 99555829226.40225000]' AS JSON)",
            "[6193038.532849999, 99555829226.40224]",
        ),
        // The first digit is not one of the seventeen — rapidjson's own
        // off-by-one. Counting it would leave one digit fewer for each of
        // these to take, and move both answers.
        (
            "SELECT CAST('[945177346.838600900, 2963649940394770.84]' AS JSON)",
            "[945177346.8386009, 2963649940394770.5]",
        ),
        // The exact accumulator stops the moment it passes 2^53 rather than
        // when it runs out of digits — three fraction digits into the first
        // of these and ten into the second — and what it leaves goes through
        // the double one rounding at a time.
        (
            "SELECT CAST('[12345678901234.5678, 987119.34197208915]' AS JSON)",
            "[12345678901234.57, 987119.3419720893]",
        ),
        // A negative hands over to the double at 2^63 where a positive waits
        // for 2^64, so the same digits with a sign read differently.
        (
            "SELECT CAST('[-9945313574470124358, -116099708312755456469406171708]' AS JSON)",
            "[-9.945313574470126e18, -1.1609970831275545e29]",
        ),
        // Scaling divides by the positive power rather than multiplying by the
        // negative one, which is not the same rounding.
        ("SELECT CAST('[-408.38500, 9.63e-143]' AS JSON)", "[-408.385, 9.63e-143]"),
        // And the power comes from a table of nearest doubles; built by
        // repeated multiplication instead, this is 4.4276401100000026e271.
        ("SELECT CAST('[442764011e263]' AS JSON)", "[4.42764011e271]"),
        // Below what the table holds the scaling is done in two steps, or the
        // divisor would be zero and the answer with it.
        (
            "SELECT CAST('[9e-309, 33.76647131e-311]' AS JSON)",
            "[9e-309, 3.376647131e-310]",
        ),
        // Leading zeros in a fraction are not significant digits, so this
        // keeps seventeen real ones and not two.
        (
            "SELECT CAST('[0.000000000000000000000000021649319015913176468]' AS JSON)",
            "[2.1649319015913174e-26]",
        ),
        // And where sixteen of them already pass 2^53, not counting the zeros
        // is what leaves room for a seventeenth.
        (
            "SELECT CAST('[0.0000000000000000000944876924289080511, -0.000091118449507296030]' AS JSON)",
            "[9.448769242890803e-20, -0.00009111844950729603]",
        ),
        // An integer past what a double holds exactly is read the same way,
        // digit by digit into the double.
        (
            "SELECT CAST('[123456789012345678901234567890]' AS JSON)",
            "[1.2345678901234566e29]",
        ),
        // The exponent is measured against where the point already sits, not
        // against 308 flat: the same `e312` is out of range at one place and
        // 1e307 five places down.
        (
            "SELECT CAST('[0.00001e312, 0.00000000000000001e325]' AS JSON)",
            "[1e307, 1e308]",
        ),
        // And once past that point the rest of its digits are read but not
        // added, so an exponent longer than any integer holds still underflows
        // to zero rather than overflowing the count of it.
        (
            "SELECT CAST('[1e-99999999999999999999, 1e-2000]' AS JSON)",
            "[0.0, 0.0]",
        ),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // And what is out of range rather than a number: an exponent past what a
    // double could hold, whatever its length, and a mantissa that passes that
    // test and overflows on the scaling. `0.00001e3120` is the pair of
    // `0.00001e312` above — five places down buys five, not the whole
    // difference — and reading the ceiling as 308 flat would make it 1e307.
    // 8.3 refuses all six.
    for sql in [
        "SELECT CAST('[1e309]' AS JSON)",
        "SELECT CAST('[1e99999999999999999999]' AS JSON)",
        "SELECT CAST('[0.00000000000000001e326]' AS JSON)",
        "SELECT CAST('[0.00001e3120]' AS JSON)",
        "SELECT CAST('[1.7976931348623159e308]' AS JSON)",
        "SELECT CAST('[-1.7976931348623159e308]' AS JSON)",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 3141, "expected invalid JSON text for: {sql}");
        assert!(
            err.message
                .contains("Number too big to be stored in double."),
            "{sql} -> {}",
            err.message
        );
    }
}

#[test]
fn a_number_too_big_for_a_double_is_not_valid_json_text() {
    // serde used to refuse this on its own, reading the literal into an f64
    // and failing on the infinity. Holding a number as its digits means
    // nothing objects any more, so the parse has to — and 8.3 does: `1e2000`
    // is 3141 there and JSON_VALID answers 0.
    let (engine, mut session) = engine();

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_VALID('1e2000'), JSON_VALID('[1e2000]'), JSON_VALID('1e308')"
        ),
        vec![vec!["0", "0", "1"]]
    );
    let sql = "SELECT CAST('1e2000' AS JSON)";
    let err = engine.execute(&mut session, sql, &[]).unwrap_err();
    assert_eq!(err.code, 3141, "expected invalid JSON text for: {sql}");
}

/// What a column of nothing but NULLs is typed as once it has been through a
/// temporary table.
///
/// `MYSQL_TYPE_NULL` is the absence of a type and no temporary-table field
/// holds one, so MySQL substitutes `binary(0)` — `MYSQL_TYPE_STRING` with the
/// binary charset. Measured against 8.3 through a real prepared statement,
/// every statement below.
///
/// The one thing this engine deliberately does not reproduce: MySQL's
/// `COM_STMT_PREPARE` answers `MYSQL_TYPE_NULL` for the *outermost* set
/// operation of a statement, because that one's temporary table is not built
/// until the statement runs — while both of its protocols' result sets, and
/// its prepare metadata for every nested case below, say `binary(0)`. #0073
/// records the measurement. One answer is given on both paths here, and it is
/// the one that carries the rows.
#[test]
fn an_all_null_column_takes_the_type_its_materialisation_gives_it() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE VIEW VN AS SELECT NULL AS c");
    run(
        &engine,
        &mut session,
        "CREATE VIEW VU AS SELECT NULL AS c UNION SELECT NULL",
    );

    // Both of the engine's type paths, because they are separate functions and
    // the wire encodes from whichever one answered: `describe` fills in
    // `COM_STMT_PREPARE`, execution fills in the result set. Asserting only one
    // of them would leave the other free to drift.
    let shapes = |session: &mut Session, sql: &str| -> (String, String) {
        let described = engine
            .describe(session, sql, 0)
            .unwrap_or_else(|e| panic!("{sql}\n  -> {e}"))
            .unwrap_or_else(|| panic!("described nothing: {sql}"));
        let Outcome::Rows(executed) = run(&engine, session, sql) else {
            panic!("expected rows from: {sql}");
        };
        (
            format!("{:?}", described[0].ty),
            format!("{:?}", executed.columns[0].ty),
        )
    };

    let mut mismatches = Vec::new();
    for (sql, expected) in [
        // Nothing has materialised, so the column is still typeless.
        ("SELECT NULL", "Null"),
        // Neither has anything here: measured on 8.3, sorting, grouping,
        // de-duplicating, limiting and folding all leave `MYSQL_TYPE_NULL`
        // alone. It is the temporary table and not the extra clause that
        // changes the type, so the set operation is not just "the one with
        // more machinery in it".
        ("SELECT NULL ORDER BY 1", "Null"),
        ("SELECT NULL GROUP BY 1", "Null"),
        ("SELECT DISTINCT NULL", "Null"),
        ("SELECT NULL LIMIT 1", "Null"),
        ("SELECT COALESCE(NULL, NULL)", "Null"),
        // A set operation materialises, so it does.
        ("SELECT NULL UNION SELECT NULL", "String"),
        ("SELECT NULL UNION ALL SELECT NULL", "String"),
        ("SELECT NULL UNION SELECT NULL UNION SELECT NULL", "String"),
        ("SELECT NULL INTERSECT SELECT NULL", "String"),
        ("SELECT NULL EXCEPT SELECT NULL", "String"),
        ("(SELECT NULL) UNION (SELECT NULL)", "String"),
        ("SELECT NULL UNION SELECT NULL ORDER BY 1", "String"),
        ("SELECT NULL UNION SELECT NULL LIMIT 1", "String"),
        // Brackets round the whole statement change nothing on 8.3, at any
        // depth and with the ORDER BY on either side of them. They do change
        // the parse — a bracketed body is a `SetExpr::Query` — and matching
        // only the bare variant left these typeless where the same statement
        // written without brackets was `binary(0)`.
        ("(SELECT NULL UNION SELECT NULL)", "String"),
        ("((SELECT NULL UNION SELECT NULL))", "String"),
        ("(SELECT NULL UNION SELECT NULL) ORDER BY 1", "String"),
        ("(SELECT NULL UNION SELECT NULL LIMIT 1)", "String"),
        // And brackets do not make a plain SELECT materialise either.
        ("(SELECT NULL)", "Null"),
        ("((SELECT NULL))", "Null"),
        ("(SELECT NULL) ORDER BY 1", "Null"),
        // A chain of them materialises *once*, over all its terms together.
        // Doing it per merge made the first pair a `binary(0)` that then
        // widened against the 1 into a string, and a binary client read "1"
        // where MySQL sends the integer 1. Bracketing the pair does not
        // change the answer on 8.3 either, so it is not associativity.
        ("SELECT NULL UNION SELECT NULL UNION SELECT 1", "LongLong"),
        ("(SELECT NULL UNION SELECT NULL) UNION SELECT 1", "LongLong"),
        ("SELECT 1 UNION (SELECT NULL UNION SELECT NULL)", "LongLong"),
        (
            "(SELECT NULL UNION SELECT NULL LIMIT 1) UNION SELECT 1",
            "LongLong",
        ),
        // A derived table, a CTE and a view are the other way a result is
        // materialised, and MySQL types them the same way — with or without a
        // set operation inside, which is what says the rule is the temporary
        // table rather than the UNION.
        ("SELECT * FROM (SELECT NULL) d", "String"),
        ("SELECT * FROM (SELECT NULL UNION SELECT NULL) d", "String"),
        ("WITH c AS (SELECT NULL) SELECT * FROM c", "String"),
        (
            "WITH c AS (SELECT NULL UNION SELECT NULL) SELECT * FROM c",
            "String",
        ),
        ("SELECT * FROM VN", "String"),
        ("SELECT * FROM VU", "String"),
        // A table value constructor is the fourth relation-shaped thing, and
        // it answers both halves of the rule at once: on its own it is
        // typeless, and materialised it is `binary(0)`. It reaches neither
        // conversion by the type it folds to — the fold sees only non-NULL
        // values, so an all-NULL column is whatever the empty fold says, and
        // saying VAR_STRING there put a `varchar(512)` in front of both.
        ("VALUES ROW(NULL)", "Null"),
        ("VALUES ROW(NULL), ROW(NULL)", "Null"),
        ("SELECT * FROM (VALUES ROW(NULL)) v", "String"),
        ("SELECT * FROM (VALUES ROW(NULL), ROW(NULL)) v", "String"),
        ("VALUES ROW(NULL) UNION VALUES ROW(NULL)", "String"),
        // One typed row still settles it, in a constructor as in a union.
        ("VALUES ROW(NULL), ROW(1)", "LongLong"),
        ("SELECT * FROM (VALUES ROW(NULL), ROW(1)) v", "LongLong"),
        // A materialised branch beside an unmaterialised one keeps the
        // materialised type, because the NULL branch still contributes none.
        (
            "SELECT * FROM (SELECT NULL UNION SELECT NULL) d UNION SELECT NULL",
            "String",
        ),
    ] {
        // Collected rather than asserted per statement, so a change that
        // breaks one shape reports that shape and not merely the first one in
        // the list. Proving a mutation bites means knowing *which* claim it
        // broke, and stopping at the first mismatch hides every later one.
        let (described, executed) = shapes(&mut session, sql);
        if (described.as_str(), executed.as_str()) != (expected, expected) {
            mismatches.push(format!(
                "{sql}\n    expected {expected}, described {described}, executed {executed}"
            ));
        }
    }
    assert!(mismatches.is_empty(), "\n{}", mismatches.join("\n"));

    // `binary(0)` and not merely "some string": the type code alone is shared
    // with `CHAR`, and the charset is the difference between the two. MySQL
    // reports exactly this for a view over either statement, which is the same
    // answer `CREATE TABLE t AS SELECT NULL` writes into the table it creates.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TABLE_NAME, DATA_TYPE, COLUMN_TYPE, CHARACTER_MAXIMUM_LENGTH, IS_NULLABLE \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME IN ('VN', 'VU') \
             ORDER BY TABLE_NAME"
        ),
        vec![
            vec!["VN", "binary", "binary(0)", "0", "YES"],
            vec!["VU", "binary", "binary(0)", "0", "YES"],
        ]
    );
}

#[test]
fn a_null_branch_of_a_union_contributes_no_type() {
    // NULL is the absence of a type, not a competing one. Widening against it
    // fell through to the string fallback, so `SELECT id FROM N UNION SELECT
    // NULL` came back a VAR_STRING and a binary client read "1" where MySQL
    // sends the integer. Measured against MySQL 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE N (id INT NOT NULL, s VARCHAR(20) NOT NULL, \
                         d DECIMAL(10,2) NOT NULL, dt DATE NOT NULL, u INT UNSIGNED NOT NULL)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO N VALUES (1,'a',1.5,'2020-01-01',7)",
    );

    let shape = |session: &Session, sql: &str| -> Vec<(String, bool, bool)> {
        engine
            .describe(session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| (format!("{:?}", c.ty), !c.nullable, c.unsigned))
            .collect()
    };

    for (sql, expected) in [
        // The other branch stands, in either order, whatever kind it is —
        // and the nullability relaxes, which is the only thing NULL changes.
        (
            "SELECT id FROM N UNION SELECT NULL",
            ("Long".to_owned(), false, false),
        ),
        (
            "SELECT NULL UNION SELECT id FROM N",
            ("Long".to_owned(), false, false),
        ),
        (
            "SELECT s FROM N UNION SELECT NULL",
            ("VarString".to_owned(), false, false),
        ),
        (
            "SELECT d FROM N UNION SELECT NULL",
            ("NewDecimal".to_owned(), false, false),
        ),
        (
            "SELECT dt FROM N UNION SELECT NULL",
            ("Date".to_owned(), false, false),
        ),
        // The sign survives too.
        (
            "SELECT u FROM N UNION SELECT NULL",
            ("Long".to_owned(), false, true),
        ),
        (
            "SELECT NULL UNION SELECT u FROM N",
            ("Long".to_owned(), false, true),
        ),
        // Nothing on either side leaves nothing to widen — and then the
        // materialising gives the column a type of its own, which is
        // `an_all_null_column_takes_the_type_its_materialisation_gives_it`.
        (
            "SELECT NULL UNION SELECT NULL",
            ("String".to_owned(), false, false),
        ),
        // Two real branches still widen, and two NOT NULL ones stay NOT NULL.
        (
            "SELECT id FROM N UNION SELECT s FROM N",
            ("VarString".to_owned(), true, false),
        ),
        // An INT beside an INT UNSIGNED: the codes agree and neither range
        // holds the other, so MySQL widens. Merging the sign *before* this
        // comparison made the two sides match and left it a LONG.
        (
            "SELECT u FROM N UNION SELECT id FROM N",
            ("LongLong".to_owned(), true, false),
        ),
    ] {
        assert_eq!(shape(&session, sql), vec![expected], "{sql}");
    }

    // Mixed signs at *different* widths. Whichever side is unsigned needs one
    // size more than itself to be held signed, and the answer has to cover the
    // other side too — asking this only at equal widths described
    // `INT UNSIGNED UNION SMALLINT` as a signed LONG, which hands a binary
    // client -1 for 4294967295.
    run(
        &engine,
        &mut session,
        "CREATE TABLE W (i INT NOT NULL, sm SMALLINT NOT NULL, iu INT UNSIGNED NOT NULL, \
                         su SMALLINT UNSIGNED NOT NULL, bu BIGINT UNSIGNED NOT NULL)",
    );
    for (sql, expected) in [
        ("SELECT iu FROM W UNION SELECT sm FROM W", "LongLong"),
        ("SELECT sm FROM W UNION SELECT iu FROM W", "LongLong"),
        // A SMALLINT UNSIGNED needs nothing extra beside an INT, which already
        // covers every value it can hold.
        ("SELECT su FROM W UNION SELECT i FROM W", "Long"),
        // Past BIGINT there is no integer left to widen to.
        ("SELECT bu FROM W UNION SELECT i FROM W", "NewDecimal"),
        ("SELECT i FROM W UNION SELECT bu FROM W", "NewDecimal"),
        // Two of the same sign never need widening at all.
        ("SELECT bu FROM W UNION SELECT bu FROM W", "LongLong"),
    ] {
        assert_eq!(shape(&session, sql)[0].0, expected, "{sql}");
    }

    // The value follows the type, and only the binary protocol can show it:
    // `rows` renders every value through `as_text`, so it reads "1" whether
    // the column is a LONG or the VAR_STRING this used to be. Four bytes of
    // little-endian integer is the difference a client sees.
    let described = engine
        .describe(&session, "SELECT id FROM N UNION SELECT NULL ORDER BY 1", 0)
        .unwrap()
        .unwrap();
    let outcome = run(
        &engine,
        &mut session,
        "SELECT id FROM N UNION SELECT NULL ORDER BY 1",
    );
    let Outcome::Rows(result) = outcome else {
        panic!("expected rows")
    };
    let one = result
        .rows
        .iter()
        .find(|row| !row[0].is_null())
        .expect("the row from the non-NULL branch");
    let mut encoded = mysql_ultralight_protocol::BufWriter::new();
    one[0]
        .to_wire()
        .encode_binary(&mut encoded, described[0].ty);
    assert_eq!(
        encoded.into_inner(),
        vec![1, 0, 0, 0],
        "a LONG goes out as four bytes; a VAR_STRING would send the text \"1\""
    );
}

#[test]
fn a_scale_argument_is_not_a_value_being_measured() {
    // `ROUND(x, 2)`'s second argument says how many places to keep. Reading it
    // as if it were another value made `ROUND(NULL, 1)` a LONGLONG on the
    // strength of the 1 — MySQL types these by the *first* argument alone and
    // ignores the other entirely. Each expectation read off MySQL 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE E (i INT, d DECIMAL(10,3), f DOUBLE, s VARCHAR(20))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO E VALUES (2, 1.5, 1.5, '3')",
    );

    let codes = |session: &Session, sql: &str| -> Vec<String> {
        engine
            .describe(session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| format!("{:?}", c.ty))
            .collect()
    };

    for (sql, expected) in [
        (
            "SELECT ROUND(NULL,1), ROUND(1,1), ROUND(i,1), ROUND(d,1), ROUND(f,1), \
                    ROUND(s,1) FROM E",
            vec![
                "Double",
                "LongLong",
                "LongLong",
                "NewDecimal",
                "Double",
                "Double",
            ],
        ),
        // Not typed at all before: every one of these was a VAR_STRING.
        (
            "SELECT TRUNCATE(NULL,1), TRUNCATE(1,1), TRUNCATE(i,1), TRUNCATE(d,1), \
                    TRUNCATE(f,1) FROM E",
            vec!["Double", "LongLong", "LongLong", "NewDecimal", "Double"],
        ),
        // `MOD` is `%` spelled as a call, and the two type identically —
        // including the DOUBLE for an operand with no numeric kind. It was
        // fixed at LONGLONG whatever it was given.
        (
            "SELECT MOD(NULL,2), MOD(1,2), MOD(i,2), MOD(d,2), MOD(f,2), MOD(s,2) FROM E",
            vec![
                "Double",
                "LongLong",
                "LongLong",
                "NewDecimal",
                "Double",
                "Double",
            ],
        ),
        (
            "SELECT NULL%2, 1%2, i%2, d%2, f%2, s%2 FROM E",
            vec![
                "Double",
                "LongLong",
                "LongLong",
                "NewDecimal",
                "Double",
                "Double",
            ],
        ),
        // The second argument is ignored, which `ROUND(1, NULL)` shows from
        // the other side: a NULL there does not make the answer a DOUBLE.
        (
            "SELECT ROUND(d, i), ROUND(1, NULL), MOD(d, f), MOD(2, NULL) FROM E",
            vec!["NewDecimal", "LongLong", "Double", "Double"],
        ),
    ] {
        assert_eq!(codes(&session, sql), expected, "{sql}");
    }

    // The types are only half of it. `MOD` was typed like `%` while still
    // reading both sides as integers, so a column described NEWDECIMAL handed
    // back the 1 it got from truncating 1.5 — a type promising a fraction it
    // had just thrown away. The two spellings have to answer the same thing.
    for (call, operator) in [
        ("SELECT MOD(1.500, 2)", "SELECT 1.500 % 2"),
        ("SELECT MOD(1.5e0, 2)", "SELECT 1.5e0 % 2"),
        ("SELECT MOD(-7, 3)", "SELECT -7 % 3"),
        ("SELECT MOD(7, 0)", "SELECT 7 % 0"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, call),
            rows(&engine, &mut session, operator),
            "{call} and {operator} disagree"
        );
    }

    // And `TRUNCATE` has to run at all: it was typed here while the evaluator
    // had never heard of it, so every one of these was refused as unsupported.
    // Toward zero rather than rounding, and the places may be negative.
    for (sql, expected) in [
        ("SELECT TRUNCATE(1.567, 1)", "1.5"),
        ("SELECT TRUNCATE(1.567, 0)", "1"),
        ("SELECT TRUNCATE(-1.567, 1)", "-1.5"),
        ("SELECT TRUNCATE(1.567, -1)", "0"),
        ("SELECT TRUNCATE(15.6, -1)", "10"),
        ("SELECT TRUNCATE(7, 1)", "7"),
        ("SELECT TRUNCATE('abc', 1)", "0"),
        ("SELECT TRUNCATE(NULL, 1)", "NULL"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn a_decimal_keeps_its_scale() {
    // A DECIMAL is exact and carries a scale — `DECIMAL(10,2)` holding 1.5 *is*
    // 1.50 — and folding one through an f64 loses both the digits and the
    // count of them. Every answer measured against MySQL 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (k INT, a DECIMAL(10,2), b DECIMAL(8,3), c DECIMAL(6,0), t TINYINT)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO D VALUES (1,1.5,1.5,1.5,100),(1,2.5,2.5,2.5,100),\
                              (2,-1.005,-1.0005,-1.5,100),(3,10,10,10,100)",
    );

    // Stored at the column's scale, rounding half *away from zero* — which is
    // not what an f64 prints: -1.005 is -1.01, and 2.5 into a scale of nothing
    // is 3 rather than the 2 a banker's rounding would give.
    assert_eq!(
        rows(&engine, &mut session, "SELECT a, b, c FROM D"),
        vec![
            vec!["1.50", "1.500", "2"],
            vec!["2.50", "2.500", "3"],
            vec!["-1.01", "-1.001", "-2"],
            vec!["10.00", "10.000", "10"],
        ]
    );

    for (sql, expected) in [
        // A total keeps the scale it added up.
        ("SELECT SUM(a) FROM D", "12.99"),
        ("SELECT SUM(b) FROM D", "12.999"),
        ("SELECT SUM(c) FROM D", "13"),
        // An average gains four digits over it, which is MySQL's
        // div_precision_increment — including over an integer, where the
        // scale it starts from is nothing.
        ("SELECT AVG(a) FROM D", "3.247500"),
        ("SELECT AVG(b) FROM D", "3.2497500"),
        ("SELECT AVG(c) FROM D", "3.2500"),
        ("SELECT AVG(t) FROM D", "100.0000"),
        // By value and not by characters, which is the whole reason a decimal
        // is tagged: as strings `10.00` is the smallest of these and `2.50`
        // the largest.
        ("SELECT MIN(a) FROM D", "-1.01"),
        ("SELECT MAX(a) FROM D", "10.00"),
        // An integer column still sums to an integer.
        ("SELECT SUM(t) FROM D", "400"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // And through a group, which is where the harness saw it.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT k, SUM(a), AVG(a) FROM D GROUP BY k ORDER BY k"
        ),
        vec![
            vec!["1", "4.00", "2.000000"],
            vec!["2", "-1.01", "-1.010000"],
            vec!["3", "10.00", "10.000000"],
        ]
    );

    // Ordering, equality and DISTINCT all read a decimal as a number. Held as
    // plain text they read as characters instead, which puts `10.00` first and
    // makes `a = '2.5'` match nothing.
    assert_eq!(
        rows(&engine, &mut session, "SELECT a FROM D ORDER BY a")
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        vec!["-1.01", "1.50", "2.50", "10.00"]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT k FROM D WHERE a = '2.5'"),
        vec![vec!["1"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(DISTINCT a) FROM D"),
        vec![vec!["4"]]
    );

    // A string is still a string: `'2.5'` and `'2.50'` are two different
    // characters-long values to MySQL, and tagging decimals must not make
    // them one.
    run(&engine, &mut session, "CREATE TABLE S (s VARCHAR(10))");
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES ('2.5'),('2.50')",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM S WHERE s = '2.5'"
        ),
        vec![vec!["1"]]
    );

    // A sum over a string column is a DOUBLE in MySQL, so it gets no scale —
    // the tag is what separates the two, not the shape of the text.
    run(&engine, &mut session, "INSERT INTO S VALUES ('1.5')");
    assert_eq!(
        rows(&engine, &mut session, "SELECT SUM(s) FROM S"),
        vec![vec!["6.5"]]
    );

    // Grouping and de-duplication key on a string rather than a comparison, so
    // that string has to be one number written one way — and *only* that
    // number. Dropping the trailing zeros without the scale they stood for
    // gave 0.25, 2.50 and 25.00 all the key 25, and a GROUP BY over a money
    // column merged rows that have nothing to do with each other.
    run(&engine, &mut session, "CREATE TABLE M (a DECIMAL(10,2))");
    run(
        &engine,
        &mut session,
        "INSERT INTO M VALUES (0.25),(2.50),(25.00),(2.00)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT COUNT(DISTINCT a) FROM M"),
        vec![vec!["4"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM (SELECT a, COUNT(*) FROM M GROUP BY a) g"
        ),
        vec![vec!["4"]]
    );

    // And one number written two ways is one key: `2.50` from a scale of two
    // meets `2.5` from a scale of one, and `2.00` meets the integer 2.
    run(&engine, &mut session, "CREATE TABLE W (a DECIMAL(8,1))");
    run(&engine, &mut session, "INSERT INTO W VALUES (2.5)");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM (SELECT a FROM M UNION SELECT a FROM W) u"
        ),
        vec![vec!["4"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM (SELECT a FROM M UNION SELECT 2) u"
        ),
        vec![vec!["4"]]
    );

    // The value used to fill a column nobody supplied goes in without passing
    // the coercion, so it needs the scale and the tag of its own accord — one
    // untagged zero in the column is a second group, and it sends `SUM` over
    // every row back through the float path.
    run(
        &engine,
        &mut session,
        "CREATE TABLE F (k INT, a DECIMAL(10,2) NOT NULL)",
    );
    run(&engine, &mut session, "INSERT IGNORE INTO F (k) VALUES (1)");
    run(&engine, &mut session, "INSERT INTO F VALUES (2, 0)");
    assert_eq!(
        rows(&engine, &mut session, "SELECT k, a FROM F ORDER BY k"),
        vec![vec!["1", "0.00"], vec!["2", "0.00"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT SUM(a) FROM F"),
        vec![vec!["0.00"]]
    );
}

#[test]
fn information_schema_declares_its_own_columns() {
    // These columns were typed by looking at the rows a query happened to
    // return: all-integers meant a signed BIGINT and anything else meant a
    // VARCHAR, so a column's type moved with the data and could never be one
    // of the codes MySQL actually sends. Each expectation below was read off
    // MySQL 8.3's own declaration of the column.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL, big BIGINT UNSIGNED, s VARCHAR(9), d DATE)",
    );

    let described = engine
        .describe(
            &session,
            "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, IS_NULLABLE, \
                    CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION, NUMERIC_SCALE, \
                    DATETIME_PRECISION, ORDINAL_POSITION, COLUMN_KEY \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'T' ORDER BY ORDINAL_POSITION",
            0,
        )
        .unwrap()
        .unwrap();
    let shape: Vec<(String, bool, bool)> = described
        .iter()
        .map(|c| (format!("{:?}", c.ty), !c.nullable, c.unsigned))
        .collect();
    assert_eq!(
        shape,
        vec![
            // A varchar MySQL leaves nullable, where the three catalogue names
            // beside it are NOT NULL.
            ("VarString".to_owned(), false, false),
            // Text, which MySQL sends as a blob — never a VAR_STRING.
            ("Blob".to_owned(), false, false),
            ("Blob".to_owned(), true, false),
            ("VarString".to_owned(), true, false),
            // The four numerics, none of them the signed BIGINT they all were.
            ("LongLong".to_owned(), false, false),
            ("LongLong".to_owned(), false, true),
            ("LongLong".to_owned(), false, true),
            ("Long".to_owned(), false, true),
            ("Long".to_owned(), true, true),
            // An ENUM, which is a fixed-width STRING and not a VAR_STRING.
            ("String".to_owned(), true, false),
        ]
    );

    // A table the corpus does not reach, to show the spec is not just the one
    // query: `SEQ_IN_INDEX` is what Prisma unwraps as an integer.
    let stats = engine
        .describe(
            &session,
            "SELECT TABLE_NAME, NON_UNIQUE, SEQ_IN_INDEX, INDEX_NAME \
             FROM information_schema.statistics WHERE TABLE_SCHEMA = 'testdb'",
            0,
        )
        .unwrap()
        .unwrap();
    let codes: Vec<(String, bool)> = stats
        .iter()
        .map(|c| (format!("{:?}", c.ty), c.unsigned))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("VarString".to_owned(), false),
            // `int`, not the `bigint` every integer here used to be.
            ("Long".to_owned(), false),
            ("Long".to_owned(), true),
            ("VarString".to_owned(), false),
        ]
    );
}

#[test]
fn a_computed_column_keeps_the_unsigned_flag() {
    // A computed column had nowhere to carry the flag, so `ROW_NUMBER()` and
    // `CAST(x AS UNSIGNED)` went out signed. Measured against MySQL 8.3, and
    // the families disagree with each other on purpose — the pairs below sit
    // either side of each rule.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE G (id INT NOT NULL, u INT UNSIGNED NOT NULL, v INT NULL)",
    );
    run(&engine, &mut session, "INSERT INTO G VALUES (1,7,NULL)");

    let signs = |session: &Session, sql: &str| -> Vec<bool> {
        engine
            .describe(session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| c.unsigned)
            .collect()
    };

    for (sql, expected) in [
        // The counting windows number their rows from one.
        (
            "SELECT ROW_NUMBER() OVER (ORDER BY id), RANK() OVER (ORDER BY id), \
                    DENSE_RANK() OVER (ORDER BY id) FROM G",
            vec![true, true, true],
        ),
        // `ROW_COUNT()` answers -1 when there was nothing to report, so it is
        // not in the list with the other two.
        (
            "SELECT CONNECTION_ID(), LAST_INSERT_ID(), ROW_COUNT(), YEAR(NOW()), MONTH(NOW()) FROM G",
            vec![true, true, false, true, false],
        ),
        // A cast says the sign in the statement itself.
        (
            "SELECT CAST(1 AS UNSIGNED), CAST(1 AS SIGNED), CAST(u AS SIGNED) FROM G",
            vec![true, false, false],
        ),
        // Arithmetic keeps the flag when *either* side has it — and loses it
        // as soon as the answer stops being an integer, or is negated.
        (
            // Subtraction is in the list, and so is a negative operand beside
            // an unsigned one. Both were left off while the engine answered
            // `u - 100` with -93 and MySQL refused it with 1690; now that the
            // engine refuses it too, the flag says what MySQL says. #0069.
            //
            // `%` reads only its left operand — `u % -1` is UNSIGNED there and
            // `-1 % u` is not — which is measured and is why it is the one
            // arithmetic operator not written as "either side".
            "SELECT u, u+1, u*2, u+u, u DIV 2, u%2, u-1, u+(-100), u*(-1), \
                    u+id, -u, u/2, u+1.5, (-1)%u FROM G",
            vec![
                true, true, true, true, true, true, true, true, true, true, false, false, false,
                false,
            ],
        ),
        // MIN and MAX answer one of the rows they folded; SUM and AVG widen
        // past the column and come back signed.
        (
            "SELECT MAX(u), MIN(u), SUM(u), AVG(u), COUNT(u) FROM G",
            vec![true, true, false, false, false],
        ),
        // A conditional needs *all* of its candidates, which is where it parts
        // company with the arithmetic above: `u + 1` is unsigned and
        // `COALESCE(u, 1)` is not.
        (
            "SELECT COALESCE(u,u), COALESCE(u,1), IF(1,u,u), IF(1,u,1), \
                    GREATEST(u,u), GREATEST(u,1) FROM G",
            vec![true, false, true, false, true, false],
        ),
        (
            "SELECT ROUND(u), CEIL(u), FLOOR(u), TRUNCATE(u,0), LENGTH('a') FROM G",
            vec![true, true, true, true, false],
        ),
        (
            "SELECT CASE WHEN 1 THEN u ELSE u END, CASE WHEN 1 THEN u ELSE 1 END, u=1 FROM G",
            vec![true, false, false],
        ),
        // A NULL candidate does not count against it, and a missing `ELSE` is
        // a NULL candidate. Measured on 8.3, where reading the absent branch
        // as a signed one made the first of these signed.
        (
            "SELECT CASE WHEN 1 THEN u END, CASE WHEN 0 THEN NULL WHEN 1 THEN u END, \
                    COALESCE(NULL, u), IF(1, u, NULL), IFNULL(NULL, u) FROM G",
            vec![true, true, true, true, true],
        ),
        // A literal above the signed range is the one literal MySQL calls
        // UNSIGNED, and it is what makes `18446744073709551615 - 1` answer
        // rather than hand a binary client -2.
        (
            "SELECT 18446744073709551615, 18446744073709551615 - 1, 9223372036854775807 FROM G",
            vec![true, true, false],
        ),
        // A window reaching past its frame answers the default it was given,
        // and that `0` is signed. The *offset* is not a candidate and does not
        // count, which is measured: `LAG(u, 1)` and `NTH_VALUE(u, 1)` are both
        // UNSIGNED on 8.3 though the 1 in each is not.
        (
            "SELECT LAG(u,1,0) OVER (ORDER BY id), FIRST_VALUE(u) OVER (ORDER BY id), \
                    LAG(u,1) OVER (ORDER BY id), NTH_VALUE(u,1) OVER (ORDER BY id) FROM G",
            vec![false, true, true, true],
        ),
        // `MOD(a, b)` is `a % b` spelled as a call and takes its sign the same
        // way, from its dividend alone.
        (
            "SELECT MOD(u, -1), MOD(-1, u), ROUND(u, -1), TRUNCATE(u, -1) FROM G",
            vec![true, false, true, true],
        ),
    ] {
        assert_eq!(signs(&session, sql), expected, "{sql}");
    }

    // Found while measuring the sign beside it: a cast is NOT NULL only to a
    // numeric target. MySQL keeps the door open for a conversion to CHAR or
    // DATE that cannot be made, and this described every cast as nullable.
    let nulls: Vec<bool> = engine
        .describe(
            &session,
            "SELECT CAST(1 AS UNSIGNED), CAST(1 AS DECIMAL), CAST(1 AS CHAR), \
                    CAST(1 AS DATE), CAST(v AS SIGNED) FROM G",
            0,
        )
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| !c.nullable)
        .collect();
    assert_eq!(nulls, vec![true, true, false, false, false]);

    // And the promise has to hold for a value the cast cannot parse. MySQL
    // reads 0 out of it; answering NULL, or handing the text back unchanged,
    // contradicted the NOT NULL those targets are described with.
    run(
        &engine,
        &mut session,
        "CREATE TABLE C (s VARCHAR(20) NOT NULL)",
    );
    run(&engine, &mut session, "INSERT INTO C VALUES ('abc')");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST(s AS DOUBLE), CAST(s AS DECIMAL), CAST(s AS SIGNED) FROM C"
        ),
        vec![vec!["0", "0", "0"]]
    );
    // A decimal keeps its text only where the text really is one. Testing
    // whether it parses as a float also takes `inf` and `1e3` and hands them
    // straight back under a NEWDECIMAL, which is not a decimal any client can
    // read; MySQL answers 0 and 1000.
    //
    // And a cast declares a scale even when it does not write one — a bare
    // `DECIMAL` is `DECIMAL(10,0)` — so `-2.5` comes back rounded away from
    // zero rather than as the text it was handed. Measured on 8.3.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST('inf' AS DECIMAL), CAST('1e3' AS DECIMAL), \
                    CAST('nan' AS DECIMAL), CAST('-2.5' AS DECIMAL), \
                    CAST('3.75' AS DECIMAL(10,1)), CAST(1 AS DECIMAL(8,2))"
        ),
        vec![vec!["0", "1000", "0", "-3", "3.8", "1.00"]]
    );

    // A union is unsigned only if both branches are, in either order — the
    // flag no longer travels with the declared type that the merge clears.
    run(&engine, &mut session, "INSERT INTO G VALUES (-5, 3, NULL)");
    for (sql, expected) in [
        ("SELECT u FROM G UNION SELECT id FROM G", false),
        ("SELECT id FROM G UNION SELECT u FROM G", false),
        ("SELECT u FROM G UNION SELECT u FROM G", true),
    ] {
        assert_eq!(signs(&session, sql), vec![expected], "{sql}");
    }

    // A computed column keeps its sign through a derived table, which is
    // rebuilt from these columns rather than from the expression.
    assert_eq!(
        signs(
            &session,
            "SELECT x FROM (SELECT CAST(u AS UNSIGNED) AS x FROM G) t"
        ),
        vec![true]
    );
}

#[test]
fn a_column_that_cannot_be_null_is_described_that_way() {
    // Under-reporting here is safe and over-reporting is not — a NOT NULL flag
    // is a promise a client acts on, and Prisma maps it to a non-optional
    // field. So every rule is measured against MySQL 8.3, and the pairs below
    // are chosen to sit either side of each one.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE G (id INT NOT NULL, k INT NOT NULL, v INT NULL, s VARCHAR(20) NULL)",
    );
    run(&engine, &mut session, "INSERT INTO G VALUES (1,1,1,'a')");

    let flags = |session: &Session, sql: &str| -> Vec<bool> {
        engine
            .describe(session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| !c.nullable)
            .collect()
    };

    for (sql, expected) in [
        // COUNT counts, and counts nothing as zero. Every other aggregate is
        // nullable — even `SUM(1)`, because an empty group has nothing to sum.
        (
            "SELECT COUNT(*), COUNT(v), COUNT(DISTINCT v), SUM(v), SUM(1), MIN(v), AVG(v) FROM G",
            vec![true, true, true, false, false, false, false],
        ),
        (
            "SELECT k, COUNT(*), SUM(v) FROM G GROUP BY k",
            vec![true, true, false],
        ),
        // A ranking window numbers its rows; one that reaches past the frame
        // answers its default, and has none unless it was given one.
        (
            "SELECT ROW_NUMBER() OVER (ORDER BY id), RANK() OVER (ORDER BY id), \
                    LAG(id,1,0) OVER (ORDER BY id), LAG(id) OVER (ORDER BY id), \
                    LAG(v,1,0) OVER (ORDER BY id) FROM G",
            vec![true, true, true, false, false],
        ),
        // COALESCE takes the first argument that is not NULL, so one that
        // cannot be is enough. IF picks between its second and third and the
        // condition decides nothing, so both results have to hold.
        (
            "SELECT COALESCE(NULL,1), COALESCE(s,'x'), COALESCE(s,v), IFNULL(v,0), \
                    IF(v,1,2), IF(1,s,'x'), ISNULL(NULL), NULLIF(1,2) FROM G",
            vec![true, true, false, true, true, false, true, false],
        ),
        // Asking whether something is NULL always has an answer. A comparison
        // does not: NULL in, NULL out.
        (
            "SELECT v IS NULL, v IS NOT NULL, EXISTS(SELECT 1), v = 1, id = 1, \
                    'a' LIKE 'a', s LIKE 'a%', 1 IN (1,2), v IN (1,2) FROM G",
            vec![true, true, true, false, true, true, false, true, false],
        ),
        // A column carries its own answer, which is why arithmetic over a
        // NOT NULL column is NOT NULL. Division is not: a zero divisor is NULL
        // however sound the operands are.
        (
            "SELECT 1, NULL, id, v, id+1, v+1, id*2, id/2, -id, -v FROM G",
            vec![
                true, false, true, false, true, false, true, false, true, false,
            ],
        ),
        // The function split is not the obvious one, and neither half was
        // guessed: `LENGTH('a')` holds where `UPPER('a')` does not, and
        // `UNIX_TIMESTAMP()` holds where `UNIX_TIMESTAMP(NULL)` cannot.
        (
            "SELECT LENGTH('a'), LENGTH(s), ABS(1), ABS(v), CEIL(1), CEIL(v), \
                    UPPER('a'), CONCAT('a','b'), MOD(4,2) FROM G",
            vec![true, false, true, false, true, false, false, false, false],
        ),
        (
            "SELECT NOW(), UNIX_TIMESTAMP(), VERSION(), UNIX_TIMESTAMP(NULL), DATABASE() FROM G",
            vec![true, true, true, false, false],
        ),
        // Only a CASE's results decide, and only with an ELSE: matching no
        // branch answers NULL. A CAST is nullable whatever it casts.
        (
            "SELECT CASE WHEN 1 THEN 1 ELSE 2 END, CASE WHEN 1 THEN v ELSE 2 END, \
                    CASE WHEN 1 THEN 1 END, CAST(1 AS CHAR) FROM G",
            vec![true, false, false, false],
        ),
    ] {
        assert_eq!(flags(&session, sql), expected, "{sql}");
    }

    // The flag is a promise the evaluator has to keep. Describing `-s` over a
    // NOT NULL column as NOT NULL and then answering NULL puts a NULL in a
    // field a client has been told is not optional — so unary minus reads the
    // number out of its operand, as the binary arithmetic beside it does.
    run(
        &engine,
        &mut session,
        "CREATE TABLE U (s VARCHAR(20) NOT NULL, b BLOB NOT NULL, n INT NOT NULL)",
    );
    run(&engine, &mut session, "INSERT INTO U VALUES ('abc','xx',5)");
    assert_eq!(
        rows(&engine, &mut session, "SELECT -s, -b, -n, +s FROM U"),
        vec![vec!["-0", "-0", "-5", "abc"]]
    );
    assert_eq!(
        flags(&session, "SELECT -s, -b, -n FROM U"),
        vec![true, true, true],
        "the promise and the value have to agree"
    );

    // `IF` at the wrong arity has no branches to check, and "every one of
    // none" is true — which promised NOT NULL for a call that answers NULL.
    assert_eq!(
        flags(&session, "SELECT IF(v), IF(v,1) FROM G"),
        vec![false, false]
    );
}

#[test]
fn a_document_compares_as_a_document() {
    // The engine compared JSON by its rendering, which is wrong in both
    // directions: `doc = '{"a": 1}'` came out true whenever the literal
    // happened to match MySQL's spacing, and the ordering was alphabetical
    // rather than by JSON type. Every answer here measured on MySQL 8.3.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE D (id INT, doc JSON)");
    run(
        &engine,
        &mut session,
        r#"INSERT INTO D VALUES (1,'{"a":1}'),(2,'{"b":2,"a":1}'),(3,'[1,2]'),
                                (4,'"hi"'),(5,'7'),(6,'true'),(7,'null')"#,
    );

    // A string operand is a JSON *string*, so it never equals an object —
    // whichever way the object is spelled. This was the headline of #0045.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT id FROM D WHERE doc = '{"a": 1}'"#
        ),
        Vec::<Vec<String>>::new()
    );
    // Cast to a document it matches, and only the document that says the same
    // thing — key order and spacing are settled on the way in.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            r#"SELECT id FROM D WHERE doc = CAST('{"a":1}' AS JSON)"#
        ),
        vec![vec!["1"]]
    );

    // A scalar operand becomes the JSON scalar of its own type.
    for (sql, expected) in [
        ("SELECT id FROM D WHERE doc = 'hi'", "4"),
        ("SELECT id FROM D WHERE doc = 7", "5"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // Ascending by JSON type, not by the alphabet. A boolean sorting after an
    // array is the surprising one, and it is what MySQL does.
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM D ORDER BY doc, id")
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
        vec!["7", "5", "4", "1", "2", "3", "6"]
    );

    // Within a type: numerically, bytewise, element-wise, fewer keys first.
    for (sql, expected) in [
        ("SELECT CAST('1' AS JSON) = CAST('1.0' AS JSON)", "1"),
        (
            "SELECT CAST('\"Hi\"' AS JSON) = CAST('\"hi\"' AS JSON)",
            "0",
        ),
        (
            "SELECT CAST('{\"A\":1}' AS JSON) = CAST('{\"a\":1}' AS JSON)",
            "0",
        ),
        (
            "SELECT CAST('[1,2]' AS JSON) < CAST('[1,2,3]' AS JSON)",
            "1",
        ),
        (
            "SELECT CAST('{\"a\":1}' AS JSON) < CAST('{\"a\":1,\"b\":2}' AS JSON)",
            "1",
        ),
        // Keys are compared as *stored* — by length then bytewise — so the
        // second object begins with `c` and this is true even though `aa`
        // sorts before `b`.
        (
            "SELECT CAST('{\"b\":1,\"c\":1}' AS JSON) < CAST('{\"aa\":9,\"c\":1}' AS JSON)",
            "1",
        ),
        ("SELECT CAST('false' AS JSON) < CAST('true' AS JSON)", "1"),
        // Member by member, key *then* that member's value. Comparing every
        // key and only then every value reaches `b` against `z` and calls the
        // left one smaller; MySQL stops at the shared first key and lets the
        // 9 and the 1 decide.
        (
            r#"SELECT CAST('{"a":9,"b":1}' AS JSON) < CAST('{"a":1,"z":2}' AS JSON)"#,
            "0",
        ),
        // Two integers compare exactly. Sent through f64 these are one number,
        // which loses a DISTINCT row and rejects a good insert into a unique
        // column.
        (
            "SELECT CAST('9007199254740993' AS JSON) = CAST('9007199254740992' AS JSON)",
            "0",
        ),
        (
            "SELECT CAST('9007199254740993' AS JSON) > CAST('9007199254740992' AS JSON)",
            "1",
        ),
        // Negative zero is zero, and the key has to agree or DISTINCT splits
        // what WHERE joins.
        ("SELECT CAST('-0.0' AS JSON) = CAST('0' AS JSON)", "1"),
        // A JSON null is a value that is present, not a missing one.
        ("SELECT CAST('null' AS JSON) IS NULL", "0"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // DISTINCT keys on a string rather than a comparison, so it needs one that
    // agrees: case is kept where a SQL string would fold it, and `1` and `1.0`
    // are one number where `'1'` and `'1.0'` are two strings.
    run(&engine, &mut session, "CREATE TABLE K (doc JSON)");
    run(
        &engine,
        &mut session,
        r#"INSERT INTO K VALUES ('{"A":1}'),('{"a":1}'),('1'),('1.0'),('"Hi"'),('"hi"')"#,
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM (SELECT DISTINCT doc FROM K) x"
        ),
        vec![vec!["5"]]
    );

    // The same key, on the two numeric cases the comparison had to be taught:
    // every spelling of zero is one row, and two integers a double cannot tell
    // apart are two.
    run(&engine, &mut session, "CREATE TABLE N (doc JSON)");
    run(
        &engine,
        &mut session,
        "INSERT INTO N VALUES ('-0.0'),('0'),('0.0'),\
                              ('9007199254740993'),('9007199254740992')",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM (SELECT DISTINCT doc FROM N) x"
        ),
        vec![vec!["3"]]
    );
}

#[test]
fn a_dedicated_node_is_visible_to_every_walk_over_an_expression() {
    // `references_column`, `contains_aggregate` and the ONLY_FULL_GROUP_BY
    // check each enumerated the variants they knew and ended in a catch-all,
    // so a variant missing from one was invisible rather than an error. They
    // had drifted more than a dozen behind the evaluator: `TRIM(x)` passed
    // ONLY_FULL_GROUP_BY and answered with an arbitrary row, and an aggregate
    // inside one of these was found and then never folded, so the raw call
    // reached the function table and was reported as unsupported.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE P (g INT, x VARCHAR(20))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO P VALUES (1,'aa'),(1,'bb'),(2,'cc')",
    );

    // A loose column inside one of these is still loose.
    for sql in [
        "SELECT SUBSTRING(x,1,1) FROM P GROUP BY g",
        "SELECT TRIM(x) FROM P GROUP BY g",
        "SELECT POSITION('a' IN x) FROM P GROUP BY g",
        "SELECT CEIL(LENGTH(x)) FROM P GROUP BY g",
        "SELECT x LIKE 'a%' FROM P GROUP BY g",
        "SELECT x IN ('aa') FROM P GROUP BY g",
    ] {
        let error = engine
            .execute(&mut session, sql, &[])
            .expect_err("the column is not grouped");
        assert_eq!(error.code, 1055, "{sql}");
        // Names the whole select item, not the leaf the walk stopped at:
        // recursing all the way down and then reporting from there said `x`
        // for `SUBSTRING(x, 1, 1)`, less than the check knew before it started.
        assert!(
            !error.message.contains("Expression 'x' is"),
            "{sql} reported only the leaf: {}",
            error.message
        );
    }

    // A grouped column is fine however deeply it is wrapped — recursing
    // through a hand-picked few variants refused these.
    for sql in [
        "SELECT FLOOR(g) + 0 FROM P GROUP BY g",
        "SELECT CEIL(g) FROM P GROUP BY g",
        "SELECT UPPER(g) FROM P GROUP BY g",
        "SELECT SUBSTRING(g,1,1) FROM P GROUP BY g",
    ] {
        assert!(engine.execute(&mut session, sql, &[]).is_ok(), "{sql}");
    }

    // An aggregate inside one is folded, so the two spellings of the same
    // function agree rather than one reporting `sum()` as unsupported.
    for sql in [
        "SELECT CEIL(SUM(g)) FROM P",
        "SELECT CEILING(SUM(g)) FROM P",
    ] {
        assert_eq!(rows(&engine, &mut session, sql), vec![vec!["4"]], "{sql}");
    }
    assert_eq!(
        rows(&engine, &mut session, "SELECT SUBSTRING(MAX(x),1,1) FROM P"),
        vec![vec!["c"]]
    );
}

#[test]
fn a_non_ascii_literal_does_not_abort_the_process() {
    // Naming a computed column walks the statement text looking for where the
    // expression ends, and it walked it a byte at a time — so a multi-byte
    // character was indexed into the middle of and the process aborted.
    // `panic = "abort"` is in the release profile, so one accented literal
    // from any client stopped the server for all of them.
    let (engine, mut session) = engine();
    assert_eq!(
        rows(&engine, &mut session, "SELECT CONCAT('café', 'ü')"),
        vec![vec!["caféü"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT 'naïve' = 'naïve', LENGTH('café')"
        ),
        vec![vec!["1", "5"]]
    );
}

#[test]
fn ceil_and_floor_are_reachable_from_sql() {
    // sqlparser gives CEIL and FLOOR their own nodes rather than parsing them
    // as calls, and nothing routed those nodes anywhere — so `SELECT CEIL(1.5)`
    // was refused outright while the `"ceil"` entry in the function table sat
    // unreachable. Code that reads as implemented but cannot be called is
    // worse than an obvious gap. `CEILING` is the same function to MySQL and
    // does parse as a call, which is why only it worked.
    //
    // Through SQL rather than `functions::call`, which is what let the gap
    // hide: a test that calls the table directly passes either way.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE E (i INT, d DECIMAL(10,3), f DOUBLE, s VARCHAR(20))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO E VALUES (2, 1.500, 1.5, '3.2')",
    );

    for (sql, expected) in [
        ("SELECT CEIL(1.5)", "2"),
        ("SELECT CEILING(1.5)", "2"),
        ("SELECT FLOOR(1.5)", "1"),
        // Away from zero and toward it, which a truncation would get wrong.
        ("SELECT CEIL(-1.5)", "-1"),
        ("SELECT FLOOR(-1.5)", "-2"),
        ("SELECT CEIL(2)", "2"),
        ("SELECT CEIL(NULL)", "NULL"),
        ("SELECT FLOOR(NULL)", "NULL"),
        // The number MySQL reads out of the argument, so a string with no
        // number in it is zero rather than NULL.
        ("SELECT CEIL('3.2')", "4"),
        ("SELECT FLOOR('abc')", "0"),
        ("SELECT ABS('abc')", "0"),
        ("SELECT ROUND('abc')", "0"),
        ("SELECT CEIL(d) FROM E", "2"),
        ("SELECT FLOOR(d) FROM E", "1"),
        // Whole, but not always an integer: forcing the answer through an
        // `as i64` saturated, so this said 9223372036854775807. MySQL types
        // an approximate argument as a DOUBLE for exactly this reason.
        ("SELECT FLOOR(1e30) > 1e29", "1"),
        ("SELECT ROUND(1e30) > 1e29", "1"),
        ("SELECT CEIL('1e400') > 1e307", "1"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // Typed by the argument, not fixed. The answer is whole, so an exact
    // argument gives an integer — but a DOUBLE argument gives a DOUBLE holding
    // a whole number, and anything with no numeric kind of its own is a DOUBLE
    // too. `ABS` differs on the decimal because its answer keeps a fraction.
    let described = engine
        .describe(
            &session,
            "SELECT CEIL(i), CEIL(d), CEIL(f), CEIL(s), CEILING(f), FLOOR(d), \
                    ABS(d), ABS(s), ROUND(d, 1), CEIL(NULL) FROM E",
            0,
        )
        .unwrap()
        .unwrap();
    let codes: Vec<String> = described.iter().map(|c| format!("{:?}", c.ty)).collect();
    assert_eq!(
        codes,
        vec![
            "LongLong",
            "LongLong",
            "Double",
            "Double",
            "Double",
            "LongLong",
            "NewDecimal",
            "Double",
            "NewDecimal",
            "Double",
        ]
    );
}

#[test]
fn a_path_extraction_binds_as_tightly_as_a_column_does() {
    // MySQL treats `->` and `->>` as primaries; sqlparser gives them nearly
    // the loosest precedence there is, so `doc->'$.b' = 6` arrives as
    // `doc -> ('$.b' = 6)` and asks for the path `0`. Silent wherever it is not
    // an outright error: a WHERE over an extraction matched nothing.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE JJ (id INT, doc JSON)");
    run(
        &engine,
        &mut session,
        "INSERT INTO JJ VALUES (1, '{\"a\":6,\"b\":2}')",
    );

    for (sql, expected) in [
        ("SELECT doc->'$.b' = 2 FROM JJ", "1"),
        ("SELECT doc->>'$.a' = '6' FROM JJ", "1"),
        ("SELECT doc->'$.b' IS NULL FROM JJ", "0"),
        ("SELECT doc->'$.b' IN (2, 3) FROM JJ", "1"),
        ("SELECT doc->'$.b' + 1 FROM JJ", "3"),
        // Left-associative, like every other chain.
        ("SELECT doc->'$.a'->>'$' FROM JJ", "6"),
        // A path extraction binds tighter than DIV, so it is the divisor
        // rather than the thing divided.
        ("SELECT 12 DIV doc->'$.b' FROM JJ", "6"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // The whole point: a filter over an extraction finds its row.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM JJ WHERE 12 DIV doc->'$.b' = 6"
        ),
        vec![vec!["1"]]
    );
}

#[test]
fn a_computed_column_is_named_after_the_text_it_was_written_as() {
    // MySQL names an unaliased select item with its source text, character for
    // character — it does not canonicalise. Rendering the parsed tree instead
    // cannot be made to agree, because spellings MySQL names differently share
    // one tree: `1!=2` and `1 <> 2` are both `NotEq`. A client keys a row by
    // this name, so a test reading the column back finds nothing.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE J (doc JSON)");

    for sql in [
        // The arrows, which the renderer spaced out every time.
        "SELECT doc->>'$.b' FROM J",
        "SELECT doc ->> '$.b' FROM J",
        "SELECT doc->'$.a' FROM J",
        // Spellings that share a tree with another spelling.
        "SELECT 1!=2",
        "SELECT 1 <> 2",
        // Spacing the renderer normalises away, or invents.
        "SELECT 1  +  2",
        "SELECT ( 1 + 2 ) * 3",
        "SELECT (1+2)*3",
        "SELECT CONCAT('a','b')",
        "SELECT CONCAT('a', 'b')",
        "SELECT IF(1,2,3)",
        // The parser reports a span covering only the arguments for these,
        // because it gives them their own grammar rather than a call.
        "SELECT SUBSTRING('abc',2)",
        "SELECT TRIM(' a ')",
        // And a span short of the closing delimiters for these.
        "SELECT COUNT(*) FROM J",
        "SELECT CAST(1 AS CHAR)",
        "SELECT JSON_EXTRACT('{}','$.a')",
        "SELECT DATE_ADD(NOW(), INTERVAL 1 DAY)",
        "SELECT CASE WHEN 1 THEN 2 ELSE 3 END",
        "SELECT 1 BETWEEN 1 AND 2",
        "SELECT doc IS NOT NULL FROM J",
        "SELECT -(1+2)",
    ] {
        let written = sql.strip_prefix("SELECT ").unwrap();
        let written = written.strip_suffix(" FROM J").unwrap_or(written);
        let described = engine.describe(&session, sql, 0).unwrap().unwrap();
        assert_eq!(described[0].name, written, "{sql}");
    }

    // An alias still wins, and a plain column is named without its table.
    let described = engine
        .describe(&session, "SELECT 1 + 1 AS total, J.doc FROM J", 0)
        .unwrap()
        .unwrap();
    let names: Vec<&str> = described.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["total", "doc"]);
}

/// Every expected name here was read off MySQL 8.3, because the rule is not
/// one anybody would guess: a literal is the single place MySQL does *not*
/// name an unaliased column after its source text.
#[test]
fn a_literal_column_is_named_by_its_contents() {
    let (engine, mut session) = engine();
    for (sql, expected) in [
        // A string is named by what it holds, not by the text that spelled it
        // — so the quotes go, a doubled quote is one quote, and an escape is
        // the character it stands for.
        ("SELECT 'abc'", "abc"),
        ("SELECT \"abc\"", "abc"),
        ("SELECT ''", ""),
        ("SELECT 'it''s'", "it's"),
        ("SELECT 'a\\'b'", "a'b"),
        ("SELECT 'a\\nb'", "a\nb"),
        ("SELECT N'a'", "a"),
        // The *first* of an adjacent run: MySQL joins the run into one string
        // only after the column already has its name.
        ("SELECT 'a' 'b' 'c'", "a"),
        ("SELECT 'a'  'b'", "a"),
        // A run whose first literal is escaped, which is the shape that
        // defeats reading a run off the lengths alone: the doubled quote
        // makes the first token one byte longer than what it decodes to and
        // the appended `'b'` makes the joined value one byte longer than the
        // first, so the two balance and only the text tells them apart.
        ("SELECT 'a''a' 'b'", "a'a"),
        ("SELECT 'a\\'a' 'b'", "a'a"),
        ("SELECT 'a' 'a''a'", "a"),
        ("SELECT '' 'x'", ""),
        ("SELECT '' ''", ""),
        ("SELECT 'aa' ''", "aa"),
        // Leading characters that are not printable ASCII go; trailing ones
        // stay, which is not symmetric and is what 8.3 does.
        ("SELECT '  a'", "a"),
        ("SELECT 'a  '", "a  "),
        ("SELECT '   '", ""),
        ("SELECT ' a '", "a "),
        // Parentheses and unary plus hand the literal straight back, however
        // many of them there are, so the literal's own name is what surfaces.
        ("SELECT ('abc')", "abc"),
        ("SELECT ( 'abc' )", "abc"),
        ("SELECT (('abc'))", "abc"),
        ("SELECT +'abc'", "abc"),
        ("SELECT + +'abc'", "abc"),
        ("SELECT +('abc')", "abc"),
        ("SELECT + 'a' 'b'", "a"),
        // A number is named by the token as written, and the plus in front of
        // it is not part of that token.
        ("SELECT 1", "1"),
        ("SELECT +1", "1"),
        ("SELECT + 1", "1"),
        ("SELECT ++1", "1"),
        ("SELECT +1.5", "1.5"),
        ("SELECT +.5", ".5"),
        ("SELECT +1e3", "1e3"),
        ("SELECT +1E3", "1E3"),
        ("SELECT ( +1 )", "1"),
        ("SELECT +( 1 )", "1"),
        ("SELECT (1)", "1"),
        // NULL is the one literal MySQL renames rather than quoting back.
        ("SELECT NULL", "NULL"),
        ("SELECT null", "NULL"),
        ("SELECT NuLl", "NULL"),
        ("SELECT +NULL", "NULL"),
        ("SELECT (null)", "NULL"),
        // The boundary. Unary minus is an operator MySQL records, so the name
        // is the text again — down to the space, which no literal name has.
        ("SELECT -'abc'", "-'abc'"),
        ("SELECT -1", "-1"),
        ("SELECT - 1", "- 1"),
        ("SELECT -(1)", "-(1)"),
        ("SELECT -1e3", "-1e3"),
        // A keyword boolean and the byte literals carry no parse-time name,
        // which is why the plus stays on these and not on a number.
        ("SELECT TRUE", "TRUE"),
        ("SELECT true", "true"),
        ("SELECT +TRUE", "+TRUE"),
        ("SELECT X'41'", "X'41'"),
        ("SELECT +X'41'", "+X'41'"),
        ("SELECT 0x41", "0x41"),
        ("SELECT +0x41", "+0x41"),
        // Peeling chooses which item to ask, never the name: with anything
        // but a literal underneath, the whole written text is the name again.
        ("SELECT +(1+2)", "+(1+2)"),
        ("SELECT (1+2)", "(1+2)"),
        ("SELECT +-1", "+-1"),
        ("SELECT -+1", "-+1"),
        ("SELECT 1 + +1", "1 + +1"),
        ("SELECT 'a' = 'a'", "'a' = 'a'"),
        ("SELECT 'a' 'b' = 'ab'", "'a' 'b' = 'ab'"),
        ("SELECT 'a' IS NULL", "'a' IS NULL"),
        ("SELECT CONCAT('a')", "CONCAT('a')"),
        ("SELECT BINARY 'a'", "BINARY 'a'"),
        ("SELECT (SELECT 'abc')", "(SELECT 'abc')"),
        // An alias still wins over all of it.
        ("SELECT 'abc' AS x", "x"),
        // And the name a client keys by is the same one whether the statement
        // was prepared or run, through a derived table, a CTE and a union.
        ("SELECT * FROM (SELECT 'abc') t", "abc"),
        ("SELECT * FROM (SELECT +'abc') t", "abc"),
        ("WITH c AS (SELECT 'abc') SELECT * FROM c", "abc"),
        ("SELECT 'a' UNION SELECT 'b'", "a"),
        ("SELECT 'a' 'b' UNION SELECT 'c'", "a"),
    ] {
        let described = engine.describe(&session, sql, 0).unwrap().unwrap();
        assert_eq!(described[0].name, expected, "describing {sql}");
        let Outcome::Rows(executed) = run(&engine, &mut session, sql) else {
            panic!("expected rows from {sql}");
        };
        assert_eq!(executed.columns[0].name, expected, "running {sql}");
    }
}

/// A view is the fourth path a name has to survive, and the only one where it
/// used to be re-derived from text the engine wrote rather than text the
/// client did.
#[test]
fn a_view_keeps_the_name_its_definition_was_written_with() {
    let (engine, mut session) = engine();
    for (body, expected) in [
        // sqlparser renders an adjacent run as the one string it denotes, so
        // a view stored as the rendered tree lost the boundary the name comes
        // from and came back `ab`.
        ("SELECT 'a' 'b'", "a"),
        ("SELECT +'abc'", "abc"),
        ("SELECT '  a'", "a"),
        ("SELECT 'a''b'", "a'b"),
        // The rewriter's spelling, which the rendered tree also loses: the
        // parser is handed `%` and MySQL names the column `1 MOD 3`.
        ("SELECT 1 MOD 3", "1 MOD 3"),
        // A union takes its names from its first branch, so that is the
        // branch the definition has to pin.
        ("SELECT 'a' 'b' UNION SELECT 'c'", "a"),
        // A parenthesised body, which is its own node in the tree.
        ("(SELECT 'a' 'b')", "a"),
        // And the names a `SELECT *` reads out of something further down.
        // These are the shapes the top-level projection cannot reach: it is a
        // wildcard, so there is nothing there to pin.
        ("SELECT * FROM (SELECT 'a' 'b') d", "a"),
        ("SELECT d.* FROM (SELECT +'abc') d", "abc"),
        ("SELECT * FROM (SELECT 'a' 'b' UNION SELECT 'c') d", "a"),
        ("WITH c AS (SELECT 'a' 'b') SELECT * FROM c", "a"),
        (
            "WITH c AS (SELECT 'a' 'b') SELECT * FROM (SELECT * FROM c) d",
            "a",
        ),
        // Both a CTE and a body with something to pin, so neither walk may
        // stop because the other one already found something.
        ("WITH c AS (SELECT 'a' 'b') SELECT 'x' 'y' FROM c", "x"),
        // Names that have to be escaped to be written back as an identifier.
        // The first is pinned, because one backtick doubles cleanly. The
        // second is not: the renderer sees a doubled backtick, decides the
        // value is already escaped and leaves it, and the alias would read
        // back as one backtick — so the guard rejects it and the view falls
        // back to text that happens to name it correctly anyway. Both answers
        // are MySQL's, by two different routes.
        ("SELECT 'a`b'", "a`b"),
        ("SELECT 'a``b'", "a``b"),
        ("SELECT 'a\\\\b'", "a\\b"),
    ] {
        run(&engine, &mut session, "DROP VIEW IF EXISTS V");
        run(&engine, &mut session, &format!("CREATE VIEW V AS {body}"));
        let described = engine
            .describe(&session, "SELECT * FROM V", 0)
            .unwrap()
            .unwrap();
        assert_eq!(described[0].name, expected, "describing view over {body}");
        let Outcome::Rows(executed) = run(&engine, &mut session, "SELECT * FROM V") else {
            panic!("expected rows from a view over {body}");
        };
        assert_eq!(
            executed.columns[0].name, expected,
            "reading view over {body}"
        );
        // Inline and through the view are the same name, which is the whole
        // claim: the view used to be the one place they disagreed.
        let inline = engine.describe(&session, body, 0).unwrap().unwrap();
        assert_eq!(inline[0].name, expected, "inline {body}");
    }

    // What the pinning actually writes, and the one name it declines to
    // write. No identifier spells the empty string, so a view over `SELECT ''`
    // is stored unpinned rather than with an empty alias MySQL would refuse to
    // read back. MySQL names that column `Name_exp_1` and this engine names it
    // with the empty string, which is a divergence of its own and not one to
    // paper over by inventing an identifier here.
    for (body, definition) in [
        ("SELECT 'a' 'b'", "SELECT 'ab' AS `a`"),
        ("SELECT ''", "SELECT ''"),
    ] {
        run(&engine, &mut session, "DROP VIEW IF EXISTS V");
        run(&engine, &mut session, &format!("CREATE VIEW V AS {body}"));
        assert_eq!(
            rows(
                &engine,
                &mut session,
                "SELECT VIEW_DEFINITION FROM information_schema.VIEWS WHERE TABLE_NAME = 'V'"
            ),
            vec![vec![definition.to_owned()]],
            "{body}"
        );
    }

    // An explicit column list still wins over the pinned name.
    run(&engine, &mut session, "DROP VIEW IF EXISTS V");
    run(
        &engine,
        &mut session,
        "CREATE VIEW V (only) AS SELECT 'a' 'b'",
    );
    let described = engine
        .describe(&session, "SELECT * FROM V", 0)
        .unwrap()
        .unwrap();
    assert_eq!(described[0].name, "only");

    // A wildcard is left alone, so a view over one still grows a column when
    // the table under it does.
    run(&engine, &mut session, "CREATE TABLE T (a INT)");
    run(&engine, &mut session, "DROP VIEW IF EXISTS W");
    run(&engine, &mut session, "CREATE VIEW W AS SELECT * FROM T");
    run(&engine, &mut session, "ALTER TABLE T ADD COLUMN b INT");
    let described = engine
        .describe(&session, "SELECT * FROM W", 0)
        .unwrap()
        .unwrap();
    let names: Vec<&str> = described.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b"]);
}

/// Under a mode that makes the rewriter change the statement, the name a
/// client gets still has to be the text *it* wrote — which is the whole point
/// of naming from the source, and exactly the modes the rewriter exists for.
///
/// Every expected name below was read off MySQL 8.3 under the same mode. They
/// are not obvious: `SELECT "x" + 1` keeps its double quotes, `SELECT "a""b"`
/// keeps the doubled one, and `COUNT (*)` keeps the space the parser was not
/// allowed to see.
///
/// A bare quoted identifier is the exception on both servers, and for the same
/// reason as a literal: it is named by what it denotes rather than by how it
/// was spelled, so `SELECT "x"` is `x` and `SELECT "a""b"` is `a"b`.
#[test]
fn a_rewritten_statement_names_its_columns_after_what_the_client_wrote() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE P (id INT NOT NULL, x INT NULL, `a\"b` INT NULL, `a``b` INT NULL, \
         PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO P VALUES (1, 5, 6, 7)");
    let plain = session.config.sql_mode.clone();

    for (flags, sql, expected) in [
        // ANSI_QUOTES, where the rewrite is the same length as what it
        // replaced and reading the original at the rewrite's offsets would
        // have worked by luck.
        (
            "ANSI_QUOTES",
            r#"SELECT "x" + 1 FROM P"#,
            &[r#""x" + 1"#][..],
        ),
        (
            "ANSI_QUOTES",
            r#"SELECT 1 + "x" * 2 FROM P"#,
            &[r#"1 + "x" * 2"#],
        ),
        // And where it is not. `""` collapses to one byte, so the rewrite is
        // shorter than the original.
        (
            "ANSI_QUOTES",
            r#"SELECT "a""b" + 1 FROM P"#,
            &[r#""a""b" + 1"#],
        ),
        // A backtick doubles, so this one is longer.
        ("ANSI_QUOTES", "SELECT \"a`b\" + 1 FROM P", &["\"a`b\" + 1"]),
        // The identifier alone, named by what it denotes.
        ("ANSI_QUOTES", r#"SELECT "x" FROM P"#, &["x"]),
        ("ANSI_QUOTES", r#"SELECT "a""b" FROM P"#, &[r#"a"b"#]),
        // IGNORE_SPACE, where the rewrite removes text that has no
        // counterpart at all — and the name has to put it back.
        ("IGNORE_SPACE", "SELECT COUNT (*) FROM P", &["COUNT (*)"]),
        ("IGNORE_SPACE", "SELECT ABS (-1)", &["ABS (-1)"]),
        (
            "IGNORE_SPACE",
            "SELECT CONCAT ('a' , 'b')",
            &["CONCAT ('a' , 'b')"],
        ),
        // MySQL names the column with the newline in it, and with the comment.
        ("IGNORE_SPACE", "SELECT ABS\n(-2)", &["ABS\n(-2)"]),
        (
            "IGNORE_SPACE",
            "SELECT ABS /* c */ (-3)",
            &["ABS /* c */ (-3)"],
        ),
        // A parenthesised item written *first*, where the gap this mode closes
        // is the one after `SELECT` itself. The parser is handed
        // `SELECT(COUNT(*))`, so the `(` the name starts at follows a letter
        // in the rewrite and a space in what was written — and the backward
        // walk has to take the second answer or the name falls back to the
        // rendered tree. Anywhere but first, the character before the `(` is a
        // comma or an operator and no gap is closed at all.
        (
            "IGNORE_SPACE",
            "SELECT (COUNT (*)) FROM P",
            &["(COUNT (*))"],
        ),
        ("IGNORE_SPACE", "SELECT ( 1 + 2 )", &["( 1 + 2 )"]),
        (
            "IGNORE_SPACE",
            "SELECT (CONCAT ('a','b'))",
            &["(CONCAT ('a','b'))"],
        ),
        // Already right before the walk was widened, because a binary
        // operator's span starts at the `(` rather than inside it.
        ("IGNORE_SPACE", "SELECT (1) + 2", &["(1) + 2"]),
        // Both modes and the MOD rewrite in one statement, so every pass that
        // changes text is composed in the answer.
        (
            "ANSI_QUOTES,IGNORE_SPACE",
            "SELECT ABS (\"a`b\"), \"x\" MOD 3, LENGTH (\"a\"\"b\") FROM P",
            &["ABS (\"a`b\")", "\"x\" MOD 3", "LENGTH (\"a\"\"b\")"],
        ),
        (
            "ANSI_QUOTES,IGNORE_SPACE",
            "SELECT UPPER (\"a\"\"b\"), \"a`b\" mod 2 FROM P",
            &["UPPER (\"a\"\"b\")", "\"a`b\" mod 2"],
        ),
        // Every pass at once and the leading parenthesis too, so the closed
        // gap, the rewritten identifier and the rewritten operator are all
        // inside one name.
        (
            "ANSI_QUOTES,IGNORE_SPACE",
            "SELECT (\"x\" MOD 3) FROM P",
            &["(\"x\" MOD 3)"],
        ),
        (
            "ANSI_QUOTES,IGNORE_SPACE",
            "SELECT (LENGTH (\"a\"\"b\")) FROM P",
            &["(LENGTH (\"a\"\"b\"))"],
        ),
    ] {
        session.config.sql_mode = format!("{plain},{flags}");
        let described = engine.describe(&session, sql, 0).unwrap().unwrap();
        let names: Vec<&str> = described.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, expected, "describing {sql} under {flags}");
        let Outcome::Rows(executed) = run(&engine, &mut session, sql) else {
            panic!("expected rows from {sql}");
        };
        let names: Vec<&str> = executed.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, expected, "running {sql} under {flags}");
    }
}

/// Pins a divergence rather than claiming a fix.
///
/// A parameter is spliced into the statement text before parsing, so by the
/// time a column is named there is no placeholder left to name it after — the
/// name is the *value's*. MySQL names that column `?` and nothing else,
/// measured on 8.3 through a real prepared statement: `?` in the prepare
/// response and `?` on every execute, whatever is bound. So this engine
/// disagrees with MySQL and, worse, with itself: describing binds NULL and
/// answers `NULL` where executing answers the value.
///
/// Naming a spliced parameter `?` needs the binder to report where it wrote,
/// through a normaliser that did not preserve offsets. Half of that is now
/// there: #0060 gave the rewriter an offset mapping, so a range of the parsed
/// text can be carried back to the text it came from. What is still missing is
/// the other half — `params::bind` reporting its own substitutions in the same
/// terms, so the two compose — which is a change to the binder rather than to
/// naming, and is not made here. This test exists so the behaviour cannot
/// drift unnoticed while it waits. See also the `SELECT ?` assertion in
/// `crates/engine/tests/describe.rs`.
#[test]
fn a_bound_parameter_column_is_named_after_its_value_where_mysql_says_question_mark() {
    let (engine, mut session) = engine();

    // MySQL: `?`.
    assert_eq!(
        engine.describe(&session, "SELECT ?", 1).unwrap().unwrap()[0].name,
        "NULL"
    );

    for (bound, name) in [
        (Value::Text("hello world".to_owned()), "hello world"),
        // The literal-naming rules reach a value nobody wrote as a literal:
        // this is the leading-whitespace trim applied to a bound string.
        (Value::Text("  x".to_owned()), "x"),
        (Value::Int(7), "7"),
        (Value::Null, "NULL"),
    ] {
        let Ok(Outcome::Rows(executed)) =
            engine.execute(&mut session, "SELECT ?", std::slice::from_ref(&bound))
        else {
            panic!("expected rows for {bound:?}");
        };
        // MySQL: `?`.
        assert_eq!(executed.columns[0].name, name, "{bound:?}");
    }

    // And the 255-byte cut lands on a bound value too.
    let Ok(Outcome::Rows(executed)) =
        engine.execute(&mut session, "SELECT ?", &[Value::Text("z".repeat(300))])
    else {
        panic!("expected rows for a long bound string");
    };
    assert_eq!(executed.columns[0].name.len(), 255);

    // An alias is the one spelling that agrees with MySQL, because it never
    // looks at the value.
    let Ok(Outcome::Rows(executed)) = engine.execute(
        &mut session,
        "SELECT ? AS lit",
        &[Value::Text("q".to_owned())],
    ) else {
        panic!("expected rows for an aliased parameter");
    };
    assert_eq!(executed.columns[0].name, "lit");
}

/// MySQL reports at most 255 bytes of a generated column name, and cuts on a
/// character boundary rather than through one. Measured on 8.3, where a
/// 256-byte literal and a 256-byte call both come back 255 bytes long.
#[test]
fn a_generated_column_name_stops_at_255_bytes() {
    let (engine, mut session) = engine();
    let long = "a".repeat(300);
    let call = format!("CONCAT('{long}')");
    // Two bytes each, so 128 of them overrun 255 by one byte: MySQL reports
    // 127 characters, not 127 and a half.
    let wide = "é".repeat(128);
    for (sql, expected) in [
        (format!("SELECT '{long}'"), "a".repeat(255)),
        (format!("SELECT {call}"), call[..255].to_owned()),
        (format!("SELECT '{wide}'"), "é".repeat(127)),
        // An alias is an identifier and is not this path: it is not cut here.
        (format!("SELECT 1 AS {long}"), long.clone()),
    ] {
        let described = engine.describe(&session, &sql, 0).unwrap().unwrap();
        assert_eq!(described[0].name, expected, "describing {sql}");
        let Outcome::Rows(executed) = run(&engine, &mut session, &sql) else {
            panic!("expected rows from {sql}");
        };
        assert_eq!(executed.columns[0].name, expected, "running {sql}");
    }
}

/// `MOD` is the keyword spelling of `%`, and sqlparser's MySQL dialect
/// implements only `DIV` in its infix hook, so the rewrite hands the parser
/// the symbol instead. That is only sound if the two really are one operator
/// at one precedence, which is what the grouping cases here check. Every
/// answer was read off MySQL 8.3.
#[test]
fn mod_spelled_as_a_keyword_is_the_percent_operator() {
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT 7 MOD 3", "1"),
        ("SELECT 7 mod 3", "1"),
        // As tight as `*`, so this is `1 + (8 MOD 3)`. Rewriting the spelling
        // without that being true would give `(1 + 8) MOD 3`, which is 0.
        ("SELECT 1 + 8 MOD 3", "3"),
        ("SELECT 8 MOD 3 + 1", "3"),
        // And left-associative among the operators it ties with.
        ("SELECT 8 MOD 5 * 2", "6"),
        ("SELECT 17 MOD 7 MOD 2", "1"),
        // The call and the operator in one statement, which is the shape that
        // makes this a token pass rather than a text substitution.
        ("SELECT MOD(17, 7) MOD 2", "1"),
        ("SELECT 7 MOD (2)", "1"),
        // The remainder itself is the one `%` already computed: the sign
        // follows the dividend and a zero divisor is NULL, not an error.
        ("SELECT -7 MOD 3", "-1"),
        ("SELECT 7 MOD 0", "NULL"),
        ("SELECT NULL MOD 2", "NULL"),
        ("SELECT 7.5 MOD 2", "1.5"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // A column named `mod` is still a column. The word is the operator only
    // between two operands, so the quoted name reads as the left one here.
    run(&engine, &mut session, "CREATE TABLE M (id INT, `mod` INT)");
    run(&engine, &mut session, "INSERT INTO M VALUES (1, 9)");
    assert_eq!(
        rows(&engine, &mut session, "SELECT `mod` MOD 4 FROM M"),
        vec![vec!["1"]]
    );

    // An unaliased column is named after the text the client wrote, and that
    // text says `MOD` whatever the parser had to be handed to read it. A
    // client keys the row by this name, so getting it wrong loses the column.
    // Down to the case the client typed: MySQL names `SELECT 7 mod 3` after
    // the lowercase it was given, and this engine keeps case everywhere else.
    let described = engine
        .describe(&session, "SELECT 7 MOD 3, 1 + 8 mod 3, 7 % 3", 0)
        .unwrap()
        .unwrap();
    let names: Vec<&str> = described.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["7 MOD 3", "1 + 8 mod 3", "7 % 3"]);

    // An alias called `mod` in front of a derived table. Reading the keyword
    // before the `(` as a function call rewrote this into a syntax error.
    assert_eq!(
        rows(&engine, &mut session, "SELECT n mod FROM (SELECT 1 AS n) x"),
        vec![vec!["1"]]
    );
}

#[test]
fn integer_division_binds_as_tightly_as_multiplication() {
    // `DIV` reaches the engine mis-associated: sqlparser's dialect hook reads
    // its right operand with `parse_expr`, which takes the rest of the
    // expression, so `7 DIV 2 + 1` arrives as `7 DIV (2 + 1)`. Every answer
    // here was read off MySQL 8.3.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT 7 DIV 2 + 1", "4"),
        ("SELECT 1 + 7 DIV 2", "4"),
        // Left-associative, so this is `(7 DIV 2) DIV 2` and not `7 DIV 1`.
        ("SELECT 7 DIV 2 DIV 2", "1"),
        ("SELECT 20 DIV 2 * 5", "50"),
        // Two rotations: the corrected shape is `((7 DIV 2) * 3) + 1`.
        ("SELECT 7 DIV 2 * 3 + 1", "10"),
        ("SELECT 2 + 100 DIV 3 * 2", "68"),
        ("SELECT 100 DIV 3 - 1", "32"),
        ("SELECT 7 DIV 2 = 3", "1"),
        // Not only other operators: the hook swallows every postfix test too,
        // so reading just `Expr::BinaryOp` left `7 DIV (2 IS NULL)` — NULL,
        // where MySQL says 0. `WHERE x DIV 2 IS NULL` matched the wrong rows
        // with no error at all.
        ("SELECT 7 DIV 2 IS NULL", "0"),
        ("SELECT 7 DIV 2 IS NOT NULL", "1"),
        ("SELECT 7 DIV 2 IS TRUE", "1"),
        ("SELECT 7 DIV 2 IN (3)", "1"),
        ("SELECT 7 DIV 2 NOT IN (3)", "0"),
        ("SELECT 7 DIV 2 BETWEEN 1 AND 5", "1"),
        ("SELECT 7 DIV 2 LIKE '3'", "1"),
        ("SELECT 7 DIV 2 REGEXP '1'", "0"),
        // `||` is two operators wearing one token. Meaning OR it binds looser
        // than DIV, so the divisor comes out of it: `(7 DIV 2) || 3` is 1.
        // Under PIPES_AS_CONCAT it binds tighter and this is `7 DIV '23'`,
        // which the mode's own test covers.
        ("SELECT 7 DIV 2 || 3", "1"),
        // Parentheses are their own node, so nothing rotates through them.
        ("SELECT 7 DIV (2 + 1)", "2"),
        ("SELECT (7 DIV 2) + 1", "4"),
        // Truncation toward zero, not a floor: a floor would say -4.
        ("SELECT -7 DIV 2", "-3"),
        // Integral whatever it divided, and a zero divisor is NULL rather
        // than an error.
        ("SELECT 7.5 DIV 2", "3"),
        ("SELECT 7 DIV 2.5", "2"),
        ("SELECT 7 DIV 0", "NULL"),
        ("SELECT NULL DIV 2", "NULL"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // A quotient that will not fit is refused rather than saturated: a
    // float-to-integer cast in Rust clamps, so this answered i64::MAX where
    // MySQL refuses the statement. Reachable from a plain string now that one
    // reads as a number up to DBL_MAX.
    for sql in ["SELECT '1e400' DIV 1", "SELECT -9223372036854775808 DIV -1"] {
        let error = engine
            .execute(&mut session, sql, &[])
            .expect_err("the quotient does not fit");
        assert_eq!(error.code, 1690, "{sql}");
    }

    // The type follows the corrected shape, whose root is usually not the DIV:
    // a DIV is a LONGLONG whatever it divided, but a DIV inside a decimal
    // multiply is a decimal.
    let described = engine
        .describe(&session, "SELECT 7 DIV 2, 7.5 DIV 2, 7 DIV 2 * 1.5", 0)
        .unwrap()
        .unwrap();
    let codes: Vec<String> = described.iter().map(|c| format!("{:?}", c.ty)).collect();
    assert_eq!(codes, vec!["LongLong", "LongLong", "NewDecimal"]);
}

/// `XOR` is the third of MySQL's logical connectives and the only one whose
/// answer no single operand can settle. Every answer here was read off MySQL
/// 8.3.
#[test]
fn xor_is_true_when_exactly_one_operand_is() {
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT 1 XOR 0", "1"),
        ("SELECT 0 XOR 1", "1"),
        ("SELECT 0 XOR 0", "0"),
        ("SELECT 1 XOR 1", "0"),
        // Unknown on either side is unknown, which is where XOR parts company
        // with the other two: `0 AND NULL` is 0 and `1 OR NULL` is 1, because
        // one operand settles those. Flipping an unknown XOR operand flips the
        // answer, so nothing settles this.
        ("SELECT NULL XOR 1", "NULL"),
        ("SELECT 1 XOR NULL", "NULL"),
        ("SELECT NULL XOR 0", "NULL"),
        ("SELECT 0 XOR NULL", "NULL"),
        ("SELECT NULL XOR NULL", "NULL"),
        // The operands are truth values and not bits: `5 XOR 3` is 0 where the
        // bitwise `5 ^ 3` is 6, and a decimal and a string are read for their
        // truth the same way a `WHERE` reads them.
        ("SELECT 2 XOR 0", "1"),
        ("SELECT 0 XOR 2", "1"),
        ("SELECT 5 XOR 3", "0"),
        ("SELECT 2.5 XOR 0", "1"),
        ("SELECT 0.0 XOR 1", "1"),
        ("SELECT 'abc' XOR 1", "1"),
        ("SELECT 'abc' XOR 0", "0"),
        ("SELECT '1x' XOR 0", "1"),
        // Left-associative, which is not the same answer read the other way:
        // `1 XOR 0 XOR 1` is `(1 XOR 0) XOR 1`.
        ("SELECT 1 XOR 0 XOR 1", "0"),
        ("SELECT 1 XOR 1 XOR 1", "1"),
        ("SELECT 0 XOR 1 XOR 1", "0"),
        ("SELECT 1 XOR 0 XOR 0 XOR 0", "1"),
        // A SQL boolean where a document can tell one from the number 1, which
        // is the funnel #0083 built and this goes through.
        ("SELECT JSON_ARRAY(1 XOR 0)", "[true]"),
        ("SELECT JSON_ARRAY(0 XOR 0)", "[false]"),
        ("SELECT JSON_ARRAY(NULL XOR 1)", "[null]"),
        ("SELECT CAST('true' AS JSON) = (1 XOR 0)", "1"),
        // And an integer everywhere else. A `Value::Bool` that missed the
        // integer paths answers a float under a LONGLONG code, which is
        // invisible in a SELECT and visible on the binary protocol.
        ("SELECT -(1 XOR 0)", "-1"),
        ("SELECT (1 XOR 0) + 1", "2"),
        ("SELECT ABS(1 XOR 0)", "1"),
        ("SELECT SUM(1 XOR 0)", "1"),
        ("SELECT CONCAT(1 XOR 0)", "1"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // Described exactly as `AND` is, which was measured rather than assumed:
    // a LONGLONG, NOT NULL only when neither operand can be NULL, and never
    // UNSIGNED however unsigned an operand is.
    run(
        &engine,
        &mut session,
        "CREATE TABLE X (a INT NOT NULL, b INT NULL, u INT UNSIGNED NOT NULL)",
    );
    let sql = "SELECT 1 XOR 0, NULL XOR 1, a XOR a, a XOR b, u XOR a, a AND b FROM X";
    let described = engine.describe(&session, sql, 0).unwrap().unwrap();
    let shape: Vec<(String, bool, bool)> = described
        .iter()
        .map(|c| (format!("{:?}", c.ty), c.nullable, c.unsigned))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("LongLong".to_string(), false, false),
            ("LongLong".to_string(), true, false),
            ("LongLong".to_string(), false, false),
            ("LongLong".to_string(), true, false),
            ("LongLong".to_string(), false, false),
            ("LongLong".to_string(), true, false),
        ]
    );
    // And named after the text it was written as, spacing and case included.
    let names: Vec<&str> = described.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "1 XOR 0",
            "NULL XOR 1",
            "a XOR a",
            "a XOR b",
            "u XOR a",
            "a AND b"
        ]
    );

    // The corrected shape decides the flags too, not the root as parsed:
    // `a XOR b IS NULL` means `a XOR (b IS NULL)`, which is nullable because
    // `a` is — where anything ending in `IS NULL` is NOT NULL on its own.
    let described = engine
        .describe(
            &session,
            "SELECT b IS NULL, b XOR 1 IS NULL, a XOR 1 IS NULL FROM X",
            0,
        )
        .unwrap()
        .unwrap();
    let nullable: Vec<bool> = described.iter().map(|c| c.nullable).collect();
    assert_eq!(nullable, vec![false, true, false]);
}

/// MySQL puts `XOR` between `OR` and `AND`; sqlparser puts it between `+` and
/// `&`, which is eight levels away and on the wrong side of every comparison,
/// every bitwise operator, `AND`, and every construct that reads as a
/// comparison. So the shape that arrives means something else for nearly
/// everything it can be written beside.
///
/// Every answer here was read off MySQL 8.3, and every comment says what the
/// parsed shape would have answered instead.
#[test]
fn xor_binds_looser_than_a_comparison_and_tighter_than_or() {
    let (engine, mut session) = engine();
    for (sql, expected) in [
        // `(2 XOR 3) AND 0`, which is 0. AND binds tighter, so this is
        // `2 XOR (3 AND 0)`.
        ("SELECT 2 XOR 3 AND 0", "1"),
        ("SELECT 0 XOR 1 AND 0", "0"),
        ("SELECT 1 AND 1 XOR 1", "0"),
        ("SELECT 0 AND 1 XOR 1", "1"),
        // All three in one chain: `(0 AND (1 XOR 1)) OR 0` is 0.
        ("SELECT 0 AND 1 XOR 1 OR 0", "1"),
        // OR binds looser, which sqlparser agrees about — the control for the
        // two above.
        ("SELECT 1 OR 1 XOR 1", "1"),
        ("SELECT 1 XOR 1 OR 1", "1"),
        ("SELECT 1 XOR (1 OR 0)", "0"),
        ("SELECT (1 XOR 1) OR 1", "1"),
        // `(0 XOR 2) = 2`, which is 0: a comparison binds tighter.
        ("SELECT 0 XOR 2 = 2", "1"),
        ("SELECT 0 XOR 2 > 1", "1"),
        ("SELECT 1 XOR 2 = 2", "0"),
        // And on the other side of one: `1 = (2 XOR 3)` is 0.
        ("SELECT 1 = 2 XOR 3", "1"),
        // `(1 XOR 2) | 0` is 1, `(1 XOR 2) & 3` is 1, `(1 XOR 3) ^ 3` is 3 and
        // `(0 XOR 1) << 1` is 2: every bitwise operator binds tighter.
        ("SELECT 1 XOR 2 | 0", "0"),
        ("SELECT 1 XOR 2 & 3", "0"),
        ("SELECT 1 XOR 3 ^ 3", "1"),
        ("SELECT 0 XOR 1 << 1", "1"),
        // Arithmetic binds tighter in both readings — the control for those.
        ("SELECT 1 XOR 2 - 2", "1"),
        ("SELECT 1 XOR 2 * 0", "1"),
        ("SELECT 1 XOR 7 DIV 2", "0"),
        ("SELECT 7 DIV 2 XOR 0", "1"),
        // `NOT` binds tighter than XOR in MySQL and looser in sqlparser, so
        // the whole XOR chain arrives inside it. On a chain of exactly two
        // terms that is invisible — `¬a XOR b` and `¬(a XOR b)` are the same
        // function, NULL included — which is why these two agree either way
        // and are the control rather than the claim.
        ("SELECT NOT 0 XOR 0", "1"),
        ("SELECT 1 XOR NOT 0", "0"),
        // The equivalence stops as soon as anything is left outside the NOT.
        // `NOT 0 XOR 1 AND 0` is `(NOT 0) XOR (1 AND 0)` — the AND is *inside*
        // the XOR and outside the NOT — where the parsed
        // `NOT (0 XOR 1) AND 0` is 0. Same for a third XOR term and for OR.
        ("SELECT NOT 0 XOR 1 AND 0", "1"),
        ("SELECT NOT 0 XOR 0 AND 0", "1"),
        ("SELECT NOT 1 XOR 1 AND 1", "1"),
        ("SELECT NOT NULL XOR 1 AND 0", "NULL"),
        ("SELECT NOT 0 XOR 1 XOR 1", "1"),
        ("SELECT NOT 1 XOR 0 XOR 1", "1"),
        ("SELECT NOT 0 XOR 1 OR 0", "0"),
        ("SELECT NOT 1 AND 0 XOR 1", "1"),
        ("SELECT NOT NOT 0 XOR 1 AND 0", "0"),
        // Everything tighter than NOT stays inside it, which is the other
        // half and the reason this is a sink and not `~`'s walk: `NOT 1 + 2`
        // is `NOT (1 + 2)` in MySQL too, and pulling the `+` out would say 1.
        ("SELECT NOT 0 = 0", "0"),
        ("SELECT NOT 1 + 2", "0"),
        ("SELECT NOT 0 | 1", "0"),
        ("SELECT NOT 3 & 1", "0"),
        ("SELECT NOT 2 IS NULL", "1"),
        ("SELECT NOT 0 XOR 2 IN (2)", "0"),
        ("SELECT NOT 3 XOR 1 LIKE '1'", "1"),
        ("SELECT NOT 0 XOR 1 | 0", "0"),
        // Parentheses are their own node and nothing rotates through them.
        ("SELECT NOT (0 XOR 1) AND 0", "0"),
        // The chain the NOT is holding need not be at the root of what it
        // holds: this arrives as `NOT ((2 || NULL) IN (1))`, and only once
        // the `IN` has sunk past the `||` is there a `||` to sink the NOT
        // past. The parsed shape says 0.
        ("SELECT NOT 2 || NULL IN (1)", "NULL"),
        ("SELECT NOT NOT 1 IS NULL || NULL", "NULL"),
        // And the answer is still a SQL boolean where a document sees one.
        ("SELECT JSON_ARRAY(NOT 0 XOR 1 AND 0)", "[true]"),
        // `||` is OR under the default mode, and sqlparser reads the token at
        // the precedence it gives `*` whichever it means. So this was wrong
        // before XOR existed: `(1 || 1) AND 0` is 0.
        ("SELECT 1 || 1 AND 0", "1"),
        ("SELECT 1 OR 1 AND 0", "1"),
        ("SELECT 1 || 0 XOR 1", "1"),
        ("SELECT 1 XOR 1 || 1", "1"),
        ("SELECT 1 || 1 XOR 1", "1"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

/// The same disagreement, against the constructs that are not operators.
///
/// `IS NULL`, `IN`, `BETWEEN`, `LIKE` and `REGEXP` bind where a comparison
/// binds, and sqlparser puts every one of them below `XOR`. None of them is a
/// `BinaryOp`, so the correction the operators get never reaches them: these
/// arrive with the construct at the root of the tree and the `XOR` inside it,
/// which is the wrong way round.
///
/// Every answer was read off MySQL 8.3.
#[test]
fn a_postfix_test_binds_tighter_than_xor() {
    let (engine, mut session) = engine();
    for (sql, expected) in [
        // `(1 XOR 2) IS NULL` is 0. MySQL means `1 XOR (2 IS NULL)`.
        ("SELECT 1 XOR 2 IS NULL", "1"),
        ("SELECT 1 XOR 0 IS NULL", "1"),
        ("SELECT 1 XOR 2 IS NOT NULL", "0"),
        ("SELECT 1 XOR 2 IS TRUE", "0"),
        ("SELECT 0 XOR 2 IN (2)", "1"),
        ("SELECT 1 XOR 2 NOT IN (2)", "1"),
        ("SELECT 0 XOR 2 BETWEEN 1 AND 3", "1"),
        ("SELECT 0 XOR 'a' LIKE 'a'", "1"),
        ("SELECT 0 XOR 'a' REGEXP 'a'", "1"),
        // The operand *after* the keyword swallows one instead, which is the
        // same problem `DIV` has and takes the same walk out:
        // `'a' LIKE ('a' XOR 0)` is 0 and `1 BETWEEN 0 AND (2 XOR 1)` is 1.
        ("SELECT 'a' LIKE 'a' XOR 0", "1"),
        ("SELECT 'a' REGEXP 'a' XOR 0", "1"),
        ("SELECT 1 BETWEEN 0 AND 2 XOR 1", "0"),
        // The upper bound has to come out whole rather than be recomputed:
        // `2 BETWEEN 0 AND (3 XOR 0)` is `2 BETWEEN 0 AND 1`, which is 0.
        ("SELECT 2 BETWEEN 0 AND 3 XOR 0", "1"),
        ("SELECT 2 BETWEEN 0 AND 3 XOR 1", "0"),
        // That half needed no XOR to go wrong and was wrong before this:
        // `'a' LIKE ('a' = 1)` is 0, where LIKE and `=` tie in MySQL and are
        // read left to right.
        ("SELECT 'a' LIKE 'a' = 1", "1"),
        // The construct has to rejoin the chain rather than only come out of
        // it. `1 AND 'a' LIKE 'a' XOR 0` means `(1 AND ('a' LIKE 'a')) XOR 0`
        // — the AND stays outside the LIKE and inside the XOR, and pulling it
        // in either direction answers 0.
        ("SELECT 1 AND 'a' LIKE 'a' XOR 0", "1"),
        ("SELECT 1 XOR 2 IS NULL AND 3", "1"),
        // The same two with operands that tell the two groupings apart:
        // `0 AND (('a' LIKE 'a') XOR 1)` is 0 and `(1 XOR (2 IS NULL)) AND 0`
        // is 0, and lifting the construct out without putting it back into
        // the chain answers exactly that.
        ("SELECT 0 AND 'a' LIKE 'a' XOR 1", "1"),
        ("SELECT 1 XOR 2 IS NULL AND 0", "1"),
        // Two levels of it: the `|` has to be corrected before the `XOR` is
        // the operator to sink the `IS NULL` past.
        ("SELECT 1 XOR 2 | 3 IS NULL", "1"),
        // The other side of the same level: a comparison ties with the
        // construct and so stays outside it, where sinking it in would make
        // this `0 = (0 IS NULL)` — 1.
        ("SELECT 0 = 0 IS NULL", "0"),
        ("SELECT 0 = 0 IS TRUE", "1"),
        ("SELECT 1 | 0 IS NULL", "0"),
        // `IN`, `BETWEEN`, `LIKE` and `REGEXP` are a level tighter still:
        // MySQL's grammar makes them a `predicate`, which a comparison
        // compares *to*. So this is `2 = (2 IN (1))` — 0 — where reading them
        // at the comparison level and letting left-associativity decide gives
        // `(2 = 2) IN (1)`, which is 1. Wrong here before #0056 as well.
        ("SELECT 2 = 2 IN (1)", "0"),
        ("SELECT 2 = 2 LIKE 1", "0"),
        ("SELECT 2 = 2 REGEXP '1'", "0"),
        ("SELECT 3 = 3 BETWEEN 0 AND 1", "0"),
        // One of them holding another. `((1 XOR 2) IS NULL) IS NULL` is 0;
        // MySQL means `1 XOR ((2 IS NULL) IS NULL)`, so the inner one has to
        // give the XOR up before the outer one has a chain to sink past.
        ("SELECT 1 XOR 2 IS NULL IS NULL", "1"),
        ("SELECT 1 XOR 2 IS NULL IS NOT NULL", "0"),
        ("SELECT 0 XOR 2 IS NULL IS NULL", "0"),
        ("SELECT 1 XOR 2 IN (2) IS NULL", "1"),
        ("SELECT 1 XOR 2 IS NULL = 1", "1"),
        // A `LIKE` pattern is the one trailing operand that can swallow
        // another construct: MySQL reads it as a `simple_expr` — `2 LIKE 1+1`
        // is a 1064 there — where sqlparser reads it below `=`, `IN` and
        // `BETWEEN`. So the walk out of it has to step through a construct of
        // its own level, which left-associativity says belongs outside it.
        // `3 LIKE (('2' = 3) IN (1, 1))` is 0.
        ("SELECT 3 LIKE '2' = 3 IN (1, 1)", "1"),
        ("SELECT 0 LIKE '2' <> 2 IN (2, 1)", "1"),
        ("SELECT 2 LIKE 1 = 1", "0"),
        // And what comes out of that walk has to be regrouped, not answered:
        // the walk lifts the *leftmost* operator to the root and MySQL groups
        // by the loosest. `((1 LIKE '2') = 0) XOR (1 IS NULL)` is what this
        // means; leaving the walk's `(1 LIKE '2') = (0 XOR (1 IS NULL))` says
        // 0, and hides the XOR from the `IS NULL` that has to sink past it.
        ("SELECT 1 LIKE '2' = 0 XOR 1 IS NULL", "1"),
        ("SELECT 0 + NULL LIKE '2' = 1 XOR 3 IS NULL", "NULL"),
        ("SELECT 3 LIKE '3' <> 3 XOR NULL IS NOT NULL", "1"),
        ("SELECT 1 LIKE '3' XOR 0 + 3 IN (3, 2)", "1"),
        ("SELECT 2 | 0 LIKE 'NULL' XOR 3 IN (3, 3)", "1"),
        // The postfix tests DIV swallows, which are corrected the other way
        // round and must stay so.
        ("SELECT 7 DIV 2 IS NULL", "0"),
        ("SELECT 7 DIV 2 IN (3)", "1"),
        ("SELECT 7 DIV 2 LIKE '3'", "1"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn the_bitwise_operators_answer_over_unsigned_bits() {
    // MySQL evaluates every one of these over 64 *unsigned* bits and reports
    // the answer BIGINT UNSIGNED, which is why they are their own piece of
    // work rather than more arithmetic: `-1 & 1` is 1 and `-1 >> 1` is
    // 9223372036854775807. Every answer here was read off MySQL 8.3.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT 7 ^ 3", "4"),
        ("SELECT 7 & 3", "3"),
        ("SELECT 7 | 3", "7"),
        ("SELECT 1 << 3", "8"),
        ("SELECT 16 >> 2", "4"),
        ("SELECT -1 & 1", "1"),
        // Logical rather than arithmetic, because the operand is unsigned: a
        // sign-extending shift would answer -1.
        ("SELECT -1 >> 1", "9223372036854775807"),
        // The count is unsigned and is not reduced modulo the width: 64 shifts
        // everything out, and -1 arrives as a count of 18446744073709551615.
        ("SELECT 1 << 64", "0"),
        ("SELECT 1 << 100", "0"),
        ("SELECT 1 << -1", "0"),
        // A number is rounded to the nearest integer first, away from zero at
        // a half — 3.7 becomes 4, and -2.5 becomes -3 and then its bits.
        ("SELECT 3.7 & 1", "0"),
        ("SELECT 3.4 & 1", "1"),
        ("SELECT 2.5 & 3", "3"),
        ("SELECT -2.5 & 3", "1"),
        ("SELECT ~-3", "2"),
        ("SELECT 1.5 << 1", "4"),
        // A string is not rounded: it converts by its leading *integer*, so it
        // stops at the point and at the exponent where the number reader that
        // arithmetic uses reads on. Reading both the same way answers one of
        // them wrongly whichever way it is read.
        ("SELECT '3.7' | 0", "3"),
        ("SELECT '1e2' | 0", "1"),
        ("SELECT 'x' & 1", "0"),
        ("SELECT ' 5 ' | 0", "5"),
        ("SELECT '-3' & 255", "253"),
        ("SELECT NULL & 1", "NULL"),
        ("SELECT 1 << NULL", "NULL"),
        ("SELECT ~NULL", "NULL"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // The column says how MySQL reads those bits: LONGLONG with the UNSIGNED
    // flag, NOT NULL unless an operand can be NULL. A sign drops the flag,
    // which is MySQL's answer for `-(7 ^ 3)` too.
    let described = engine
        .describe(&session, "SELECT ~-1, 7 ^ 3, NULL & 1, -(7 ^ 3)", 0)
        .unwrap()
        .unwrap();
    let shape: Vec<String> = described
        .iter()
        .map(|c| format!("{:?} {} {}", c.ty, c.unsigned, c.nullable))
        .collect();
    assert_eq!(
        shape,
        vec![
            "LongLong true false",
            "LongLong true false",
            "LongLong true true",
            "LongLong false false",
        ]
    );
}

#[test]
fn a_bitwise_answer_too_wide_for_an_i64_is_answered() {
    // These were refused with 1235 until #0069 gave `Value` an unsigned tag,
    // which is what #0077 was waiting for. Refusing was the right answer while
    // the alternative was a wrong one: carrying the bits in an `i64` and
    // letting the column's UNSIGNED flag reinterpret them prints the right
    // digits for `SELECT ~0` and nothing else — `~0 > 5` answered 0 for
    // MySQL's 1, `CONCAT(~0)` answered -1, `~0 MOD 10` answered
    // 18446744073709551615 for 5 — because nothing on the value told the next
    // reader it was unsigned. Now something does.
    //
    // The mixed contexts are here for exactly that reason: the digits alone
    // would pass `SELECT ~0` while every one of the readings below stayed
    // wrong. All measured on 8.3.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT ~0", "18446744073709551615"),
        ("SELECT 1 << 63", "9223372036854775808"),
        ("SELECT -5 & -3", "18446744073709551609"),
        ("SELECT ~0 > 5", "1"),
        ("SELECT ~0 DIV 2", "9223372036854775807"),
        ("SELECT ~0 - 1", "18446744073709551614"),
        ("SELECT ~0 MOD 10", "5"),
        ("SELECT CONCAT(~0)", "18446744073709551615"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // And the one direction that still refuses, because it has to: a bitwise
    // answer is a BIGINT UNSIGNED, so adding one to the largest of them leaves
    // the range and MySQL raises 1690 rather than wrapping to zero.
    let error = engine
        .execute(&mut session, "SELECT ~0 + 1", &[])
        .expect_err("2^64 is not a BIGINT UNSIGNED");
    assert_eq!(error.code, 1690, "{}", error.message);
}

#[test]
fn a_bitwise_chain_composes_over_bits_it_cannot_hand_out() {
    // A chain is computed as a chain: `1 + 3 & ~5` is 0 in MySQL and reaches
    // it through 18446744073709551610. This was written when that intermediate
    // had no `Value` to be — checking each operator against `i64` refused 1354
    // of 6000 random expressions it can answer exactly — and it still holds
    // now that one does, because the composition is what makes the answers
    // right rather than merely representable.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT 1 + 3 & ~5", "0"),
        ("SELECT ~0 & 255", "255"),
        ("SELECT ~2 >> 4 - 2", "4611686018427387903"),
        ("SELECT (1 << 63) >> 63", "1"),
        ("SELECT ~-6 ^ ~-6", "0"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // Refused rather than answered numerically, and refused whichever side it
    // is on. MySQL gives these operators a second, bytewise meaning over a
    // *binary* string, and `Value::Bytes` is what both a `VARBINARY` column
    // and a hex literal come back as — which MySQL reads oppositely, 0 for
    // `_binary'ab' & 1` and 15 for `X'0F' & 255`. One value and two answers,
    // so neither is given.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Bin (a VARBINARY(4), b VARBINARY(4))",
    );
    run(&engine, &mut session, "INSERT INTO Bin VALUES ('ab', 'cd')");
    for sql in [
        "SELECT a & b FROM Bin",
        "SELECT a & 255 FROM Bin",
        "SELECT X'0F' & 255",
        "SELECT ~a FROM Bin",
    ] {
        let error = engine
            .execute(&mut session, sql, &[])
            .expect_err("a bitwise operator over a binary string is not implemented");
        assert_eq!(error.code, 1235, "{sql}");
    }
}

#[test]
fn a_bitwise_decimal_operand_is_read_exactly() {
    // #0052 measured that a *number* operand rounds to an integer away from
    // zero at a half. A decimal is one, and reaching that rule through an
    // `f64` loses it: 4503599627370496.5 is the first tie a double cannot
    // hold, and rounds to ...496 there where MySQL answers ...497. Counting
    // the digits instead is exact for anything MySQL's own DECIMAL holds.
    // Every answer here was read off MySQL 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (id INT NOT NULL, wide DECIMAL(20,1), one DECIMAL(10,1), \
         two DECIMAL(10,2), huge DECIMAL(30,0), wide30 DECIMAL(40,30), \
         carry DECIMAL(40,30))",
    );
    // Written as strings, because a decimal *literal* is an f64 to this engine
    // on the way in as much as on the way out — see #0070 — and the column is
    // what this test is about.
    run(
        &engine,
        &mut session,
        "INSERT INTO D VALUES (1, '4503599627370496.5', '3.7', '2.55', \
         '999999999999999999999999999999', '1234567890.5', \
         '9.9999999999999999999999999999995')",
    );
    for (sql, expected) in [
        // Ten integer digits at a scale of 30 is more than an `i128` counts,
        // and MySQL holds it: the digits are rescaled on the characters
        // rather than folded through an `f64`, which answered `i128::MAX`.
        (
            "SELECT wide30 FROM D",
            "1234567890.500000000000000000000000000000",
        ),
        // Including the carry, which can run off the front of the number.
        ("SELECT carry FROM D", "10.000000000000000000000000000000"),
        ("SELECT wide | 0 FROM D", "4503599627370497"),
        ("SELECT wide & 1 FROM D", "1"),
        ("SELECT one | 0 FROM D", "4"),
        ("SELECT two | 0 FROM D", "3"),
        // A decimal past 64 bits saturates at the *signed* end, which is not
        // where a string saturates: `'99999999999999999999' | 0` is the
        // unsigned maximum and this is 9223372036854775807.
        ("SELECT huge | 0 FROM D", "9223372036854775807"),
        // The cast has to carry the type too, or the operand arrives as text
        // and is read by its leading digits: this answered 3.
        ("SELECT CAST('3.7' AS DECIMAL(10,1)) | 0", "4"),
        ("SELECT CAST('2.5' AS DECIMAL(10,1)) | 0", "3"),
        // And a decimal literal is read off the token for the same reason the
        // column is read off its digits.
        ("SELECT 4503599627370496.5 | 0", "4503599627370497"),
        ("SELECT 4503599627370496.5 & 1", "1"),
        // A sign is a unary operator to the parser and part of the number to
        // MySQL, so the same digits have to read the same way with one: this
        // answered 0 and 0 while the positive twin beside it answered
        // correctly, which is the same number diverging by how it was written.
        ("SELECT -4503599627370496.5 & 1", "1"),
        ("SELECT -4503599627370496.5 & 3", "3"),
        ("SELECT -3.7 & 255", "252"),
        // A leading `+` is a unary operator too, and it has to be peeled for
        // the same reason: with it left on, the tie goes back through the f64.
        ("SELECT +4503599627370496.5 & 1", "1"),
        ("SELECT +2.5 & 3", "3"),
        // Signs are counted rather than concatenated. Spelling `- -2.5` as
        // the text `"--2.5"` made it not a decimal at all, so it fell to the
        // leading-number reader and answered 0 — a correct answer turned
        // wrong by the fix beside it.
        ("SELECT - -2.5 & 3", "3"),
        ("SELECT - - -2.5 & 3", "1"),
        ("SELECT -(-2.5) & 3", "3"),
        // Parentheses group a number without retyping it, so the exact
        // reading has to see through them.
        ("SELECT -(4503599627370496.5) & 1", "1"),
        // An exponent is a DOUBLE to MySQL rather than a decimal, so it keeps
        // the reading it had.
        ("SELECT 1e2 | 0", "100"),
        // Past the 38 digits an `i128` counts the digits are kept rather than
        // read through an `f64`, which answered `i128::MAX` — a number nothing
        // asked for. MySQL's DECIMAL goes to 65, so these are values it holds
        // and answers exactly.
        (
            "SELECT CAST('123456789012345678901234567890123456789012' AS DECIMAL(65,0))",
            "123456789012345678901234567890123456789012",
        ),
        (
            "SELECT CAST('12345678901234567890123456789012345678.12' AS DECIMAL(65,2))",
            "12345678901234567890123456789012345678.12",
        ),
        // The scale is reached exactly even where counting it would overflow
        // an `i128` — `DECIMAL(65,30)` spends the whole budget on the scale,
        // so this padded to 30 places rather than handing back `1.5`.
        (
            "SELECT CAST('1.5' AS DECIMAL(65,30))",
            "1.500000000000000000000000000000",
        ),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn a_temporal_in_arithmetic_is_its_packed_number() {
    // MySQL converts a temporal to its packed `YYYYMMDD[HHMMSS]` integer in an
    // arithmetic context, and the answer turns on the operand's *type*: the
    // DATE column and the VARCHAR beside it hold the same characters and
    // convert to 20240105 and 2024. Measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL, d DATE, dt DATETIME, dt3 DATETIME(3), \
         tm TIME, tm3 TIME(3), ts TIMESTAMP NULL, y YEAR, s VARCHAR(30), \
         short DATETIME, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES (1, '2024-01-05', '2024-01-05 06:07:08', \
         '2024-01-05 06:07:08.500', '06:07:08', '06:07:08.500', \
         '2024-01-05 06:07:08', 2024, '2024-01-05', '2024-01-05 06:07')",
    );
    for (sql, expected) in [
        ("SELECT d + 0 FROM T", "20240105"),
        ("SELECT dt + 0 FROM T", "20240105060708"),
        ("SELECT tm + 0 FROM T", "60708"),
        ("SELECT ts + 0 FROM T", "20240105060708"),
        // A YEAR is already a number and MySQL computes in it directly.
        ("SELECT y + 0 FROM T", "2024"),
        // Fractional seconds pack after the point and keep every digit the
        // column was declared with — 20240105060708.5 is not the same answer.
        ("SELECT dt3 + 0 FROM T", "20240105060708.500"),
        ("SELECT tm3 + 0 FROM T", "60708.500"),
        // Missing groups pack as zero rather than shortening the number. The
        // engine stores a DATETIME as it was written, so this column really
        // does hold `2024-01-05 06:07`.
        ("SELECT short + 0 FROM T", "20240105060700"),
        ("SELECT d - 0 FROM T", "20240105"),
        ("SELECT d * 1 FROM T", "20240105"),
        ("SELECT d MOD 7 FROM T", "4"),
        ("SELECT d DIV 1 FROM T", "20240105"),
        ("SELECT -d FROM T", "-20240105"),
        // Unary plus is the identity and does not convert at all.
        ("SELECT +d FROM T", "2024-01-05"),
        // The counterweight: the same characters in a VARCHAR, and the same
        // characters written as a literal, are strings and read 2024.
        ("SELECT s + 0 FROM T", "2024"),
        ("SELECT '2024-01-05' + 0", "2024"),
        ("SELECT '06:07:08' + 0", "6"),
        // Beside a string the temporal still converts; the string does not.
        ("SELECT d + '1' FROM T", "20240106"),
        ("SELECT d + s FROM T", "20242129"),
        // A comparison against a number is numeric, over the packed value.
        ("SELECT d = 20240105 FROM T", "1"),
        ("SELECT dt = 20240105060708 FROM T", "1"),
        ("SELECT tm = 60708 FROM T", "1"),
        ("SELECT d > 2024 FROM T", "1"),
        // And against a string it is not, so a DATE still equals its own text.
        ("SELECT d = '2024-01-05' FROM T", "1"),
        // A cast reads the packed number and rounds it away from zero.
        ("SELECT CAST(d AS SIGNED) FROM T", "20240105"),
        ("SELECT CAST(dt3 AS SIGNED) FROM T", "20240105060709"),
        ("SELECT CAST(tm3 AS SIGNED) FROM T", "60709"),
        ("SELECT CAST(d AS DECIMAL(20,2)) FROM T", "20240105.00"),
        // A cast to CHAR is the text, which is the distinction itself.
        ("SELECT CAST(d AS CHAR) FROM T", "2024-01-05"),
        ("SELECT CONCAT(d, '') FROM T", "2024-01-05"),
        // An aggregate folds the packed numbers, and MIN and MAX keep the
        // value they folded.
        ("SELECT SUM(d) FROM T", "20240105"),
        ("SELECT AVG(d) FROM T", "20240105.0000"),
        ("SELECT SUM(dt3) FROM T", "20240105060708.500"),
        ("SELECT MAX(d) FROM T", "2024-01-05"),
        ("SELECT MIN(tm) FROM T", "06:07:08"),
        // A TIME keeps its sign and runs past 99 hours.
        ("SELECT CAST('-06:07:08' AS TIME) + 0", "-60708"),
        ("SELECT CAST('838:59:59' AS TIME) + 0", "8385959"),
        // Truthiness asks the same number, which is only visible where the
        // two readings disagree about zero: `00:07:08` is 708 packed and 0 by
        // its leading digits, and MySQL calls it true.
        ("SELECT IF(CAST('00:07:08' AS TIME), 'y', 'n')", "y"),
        ("SELECT IF(CAST('00:00:00' AS TIME), 'y', 'n')", "n"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // Written in a shape the column does not use. MySQL stores a temporal by
    // its type, so both of these are canonical by the time they are read back
    // — and the packed number follows from the column rather than from how the
    // statement happened to spell it.
    run(
        &engine,
        &mut session,
        "INSERT INTO T (id, d, short) VALUES (2, '2024-1-5', '2024-01-05 6:7')",
    );
    for (sql, expected) in [
        ("SELECT d + 0 FROM T WHERE id = 2", "20240105"),
        ("SELECT short + 0 FROM T WHERE id = 2", "20240105060700"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

/// #0070. Arithmetic over a decimal went through an `f64` and came back with
/// whatever the float printed, so the scale MySQL computes was lost at every
/// operator. Every expected value below was read off MySQL 8.3.
#[test]
fn decimal_arithmetic_carries_mysql_s_scale() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (a DECIMAL(10,2), b DECIMAL(8,3))",
    );
    run(&engine, &mut session, "INSERT INTO D VALUES (1.5, 1.5)");

    for (sql, expected) in [
        // The issue's own table, and then the same operators with the operands
        // the other way round — a rule that reads the *left* scale has to be
        // asked twice or half of it goes untested.
        ("SELECT a + b FROM D", "3.000"),
        ("SELECT b + a FROM D", "3.000"),
        ("SELECT a - b FROM D", "0.000"),
        ("SELECT b - a FROM D", "0.000"),
        ("SELECT a * b FROM D", "2.25000"),
        ("SELECT b * a FROM D", "2.25000"),
        // Four places wider than the *left* operand, so this pair differs.
        ("SELECT a / b FROM D", "1.000000"),
        ("SELECT b / a FROM D", "1.0000000"),
        ("SELECT a + 1 FROM D", "2.50"),
        ("SELECT 1 + a FROM D", "2.50"),
        ("SELECT a * 2 FROM D", "3.00"),
        ("SELECT a + 0.5 FROM D", "2.00"),
        ("SELECT -a FROM D", "-1.50"),
        ("SELECT ABS(-a) FROM D", "1.50"),
        // `%` and `MOD` are the same operator and take the wider scale,
        // padding out to it rather than answering their own digits.
        ("SELECT a % b FROM D", "0.000"),
        ("SELECT a MOD b FROM D", "0.000"),
        ("SELECT 10.5 MOD 3.00", "1.50"),
        // `DIV` has no scale at all, whatever it divided.
        ("SELECT a DIV b FROM D", "1"),
        ("SELECT 7.5 DIV 2", "3"),
        ("SELECT -7.5 DIV 2", "-3"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

/// #0085. A quotient is four places wider than its left operand —
/// `div_precision_increment` — even over two plain integers, and the digits
/// come from long division rather than from a double.
#[test]
fn division_widens_by_the_precision_increment() {
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT 1/3", "0.3333"),
        ("SELECT 10/3", "3.3333"),
        ("SELECT 2/3", "0.6667"),
        ("SELECT -2/3", "-0.6667"),
        ("SELECT 7/2", "3.5000"),
        ("SELECT 1/7", "0.1429"),
        // MySQL computes the quotient to a whole number of nine-digit words
        // and truncates *there*, then rounds that to the result's scale. Five
        // places on the left asks for nine and computes nine, so the
        // truncation is what shows: 0.333336666, not 0.333336667.
        ("SELECT 1.00001/3", "0.333336666"),
        // 25 places asks for 29 and computes 36, so the far end does round.
        (
            "SELECT 1.0000000000000000000000000/7",
            "0.14285714285714285714285714286",
        ),
        // A zero divisor is NULL rather than an error, for all three.
        ("SELECT 1/0", "NULL"),
        ("SELECT 1.5 MOD 0", "NULL"),
        ("SELECT 1 DIV 0", "NULL"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // Exact past what a double holds, which is the other half of #0085: a
    // BIGINT UNSIGNED over two is not a number an `f64` can halve.
    assert_eq!(
        rows(&engine, &mut session, "SELECT 18446744073709551615 / 2"),
        vec![vec!["9223372036854775807.5000"]]
    );
}

/// A number written without an exponent is exact, and carries the scale it was
/// written with. Until #0070 it evaluated to an `f64`, so `SELECT 1.50` was
/// 1.5 and `SELECT 18446744073709551616` was 1.8446744073709552e19 — both
/// under columns this engine already described NEWDECIMAL.
#[test]
fn a_numeric_literal_keeps_every_digit_it_was_written_with() {
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT 1.50", "1.50"),
        ("SELECT 100.0", "100.0"),
        // Leading zeros go and written places stay, which is how MySQL prints
        // the same two literals.
        ("SELECT .5", "0.5"),
        ("SELECT 007.5", "7.5"),
        ("SELECT 00.00", "0.00"),
        ("SELECT 0.1 + 0.2", "0.3"),
        ("SELECT CONCAT(1.50)", "1.50"),
        // Past `u64::MAX` and still exact: MySQL types this NEWDECIMAL.
        ("SELECT 18446744073709551616", "18446744073709551616"),
        ("SELECT 18446744073709551616 + 1", "18446744073709551617"),
        ("SELECT 99999999999999999999999", "99999999999999999999999"),
        // An exponent is a DOUBLE to MySQL and stays approximate here, which
        // is the one numeric literal that does.
        ("SELECT 1.5e3", "1500"),
        ("SELECT 0.1e0 + 0.2e0", "0.30000000000000004"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

/// The type a decimal expression is *described* with, which the binary
/// protocol encodes by. A quotient is a NEWDECIMAL over exact operands and a
/// DOUBLE the moment one of them is approximate — measured on 8.3, where
/// `d / f` is DOUBLE and `d / i` is NEWDECIMAL — and describing an approximate
/// quotient NEWDECIMAL sent a client digits the column promised were exact.
#[test]
fn a_quotient_is_described_by_what_it_divided() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Q (d DECIMAL(10,2), i INT, f FLOAT, g DOUBLE)",
    );
    let codes = |sql: &str| -> Vec<String> {
        engine
            .describe(&session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| format!("{:?}", c.ty))
            .collect()
    };
    assert_eq!(
        codes("SELECT d / i, i / i, d / d, d / 2 FROM Q"),
        vec!["NewDecimal"; 4]
    );
    assert_eq!(
        codes("SELECT d / f, d / g, f / i FROM Q"),
        vec!["Double"; 3]
    );
    // An operand whose type cannot be worked out ahead of running it is not
    // *known* to be approximate, and MySQL answers a decimal when neither side
    // is: `(SELECT 1)/2` is NEWDECIMAL on 8.3. Announced DOUBLE, `reconcile`
    // parses the exact quotient back through an `f64` on its way out — which
    // is how the same expression printed 0.5 as a column and 0.5000 inside a
    // `CONCAT`.
    assert_eq!(
        codes("SELECT (SELECT 1)/2, 2/(SELECT 2), (SELECT 1)/(SELECT 2)"),
        vec!["NewDecimal"; 3]
    );
    // And a literal past both integer types is a NEWDECIMAL rather than the
    // LONGLONG its lack of a point would otherwise say.
    assert_eq!(
        codes("SELECT 18446744073709551616, 18446744073709551615, 1.5"),
        vec!["NewDecimal", "LongLong", "NewDecimal"]
    );
}

/// A decimal past what an `i128` counts. MySQL's DECIMAL is 65 digits where an
/// `i128` is 38, and the overflow used to fall back to the `f64` path — which
/// answered the right number to fifteen digits and lost the scale with it.
#[test]
fn decimal_arithmetic_reaches_past_an_i128() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE W (wide DECIMAL(65,30), narrow DECIMAL(30,10), big DECIMAL(65,0))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO W VALUES (1.5, 3.5, 99999999999999999999999999999999999)",
    );

    for (sql, expected) in [
        // 40 places asked for and 30 given, and every digit of the answer kept.
        (
            "SELECT narrow * wide FROM W",
            "5.250000000000000000000000000000",
        ),
        (
            "SELECT wide + wide FROM W",
            "3.000000000000000000000000000000",
        ),
        // 70 digits, which MySQL prints in full under a column it still
        // describes `decimal(65,0)` — the precision is not clamped, only the
        // scale is.
        (
            "SELECT big * big FROM W",
            "9999999999999999999999999999999999800000000000000000000000000000000001",
        ),
        // De-duplication keys a decimal by its number and not by its text, and
        // that has to hold at a width no `i128` counts: `2.50` and `2.5` were
        // one group already, and these two spellings of one 70-digit product
        // became two the moment arithmetic could produce them.
        (
            "SELECT COUNT(*) FROM (SELECT big * big AS v FROM W \
             UNION SELECT big * big * 1.0 FROM W) t",
            "1",
        ),
        // A BIGINT is what `DIV` answers, so one that does not fit is 1690
        // rather than a wrong number.
        ("SELECT big DIV 7 FROM W", "!1690"),
        ("SELECT 99999999999999999999999.0 DIV 1", "!1690"),
        ("SELECT 9223372036854775807.5 DIV 1", "9223372036854775807"),
        // And `DIV` keeps the UNSIGNED tag from either operand, so a decimal
        // divisor does not shrink the range the answer is checked against:
        // `18446744073709551615 DIV 1.0` is that number on 8.3, where an
        // `i64`'s range would have refused it.
        (
            "SELECT 18446744073709551615 DIV 1.0",
            "18446744073709551615",
        ),
        ("SELECT 18446744073709551615 DIV 2.0", "9223372036854775807"),
        ("SELECT 18446744073709551615 DIV 0.5", "!1690"),
    ] {
        match expected.strip_prefix('!') {
            Some(code) => {
                let error = engine.execute(&mut session, sql, &[]).expect_err(sql);
                assert_eq!(error.code.to_string(), code, "{sql}");
                assert_eq!(error.sql_state, "22003", "{sql}");
            }
            None => assert_eq!(
                rows(&engine, &mut session, sql),
                vec![vec![expected]],
                "{sql}"
            ),
        }
    }
}

/// A decimal compares as exactly as it computes. Until #0070 both were read
/// through an `f64`, so the wrongness was at least self-consistent; making
/// storage and arithmetic exact past seventeen significant digits is what made
/// a comparison that is not exact contradict the row it sits in.
#[test]
fn a_decimal_compares_as_exactly_as_it_computes() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Wd (id INT, a DECIMAL(65,0), b DECIMAL(65,0))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Wd VALUES (1, 10000000000000000000000000000000000001, \
         10000000000000000000000000000000000000)",
    );

    // The row used to say two things at once: `a - b` was 1 and `a = b` was
    // also 1, because the difference was exact and the comparison was not.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT a - b, a = b, a > b, a < b FROM Wd"
        ),
        vec![vec!["1", "0", "1", "0"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Wd WHERE a = b"),
        Vec::<Vec<String>>::new()
    );

    // Reachable with no table at all, under columns #0070 newly and correctly
    // prints every digit of.
    for (sql, expected) in [
        ("SELECT 18446744073709551616 = 18446744073709551615", "0"),
        ("SELECT 18446744073709551616 > 18446744073709551615", "1"),
        // And every spelling of one number is still one value: a scale is not
        // part of what a comparison asks, and a zero has no sign.
        ("SELECT 1.50 = 1.5", "1"),
        ("SELECT 1.50 > 1.5", "0"),
        ("SELECT -0.00 = 0", "1"),
        ("SELECT 0.0 = -0.0", "1"),
        ("SELECT 2.50 < 10.00", "1"),
        ("SELECT -2.5 < -1.5", "1"),
        // A numeric string still reads as a double, which is a different
        // question with its own answer — `d + '2.5'` is approximate too.
        ("SELECT 2.50 = '2.5'", "1"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // `=` and the GROUP BY key have to agree, and both are exact now. Keyed by
    // its text a decimal was two groups; keyed through an `f64` it was one
    // group that `=` then called two.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(DISTINCT v) FROM (SELECT a AS v FROM Wd \
             UNION ALL SELECT b FROM Wd) t"
        ),
        vec![vec!["2"]]
    );
}

/// An exact quotient survives the trip out. `ResultSet::reconcile` converts a
/// value to the code its column was announced with, so announcing a quotient
/// DOUBLE parses the exact digits back through an `f64` — and only for the
/// reader that goes through a column, which is what made one expression give
/// two answers.
#[test]
fn an_exact_quotient_is_not_announced_approximate() {
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT (SELECT 1)/2", "0.5000"),
        ("SELECT CONCAT((SELECT 1)/2)", "0.5000"),
        ("SELECT 2/(SELECT 2)", "1.0000"),
        ("SELECT (SELECT 1)/(SELECT 2)", "0.5000"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn arithmetic_over_a_temporal_is_typed_by_its_packed_number() {
    // The value and the type are decided in different code for the same
    // expression, and the binary protocol encodes by the *type* — so a column
    // holding 20240105 and described DOUBLE reaches a client as a float. Every
    // code here measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE E (d DATE, dt DATETIME, dt3 DATETIME(3), tm TIME, tm3 TIME(3), \
         y YEAR, s VARCHAR(20))",
    );
    let codes = |engine: &Engine, session: &Session, sql: &str| -> Vec<String> {
        engine
            .describe(session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| format!("{:?}", c.ty))
            .collect()
    };
    for (sql, expected) in [
        // A temporal converts to an integer before the operator sees it, so
        // the arithmetic over it is integer arithmetic.
        (
            "SELECT d + 0, dt - 0, tm * 1, d MOD 7, d DIV 1 FROM E",
            vec!["LongLong"; 5],
        ),
        // Fractional seconds pack after the point, which makes the operand a
        // decimal and the answer one.
        (
            "SELECT dt3 + 0, tm3 - 0, dt3 * 1 FROM E",
            vec!["NewDecimal"; 3],
        ),
        // Unary minus is a DOUBLE where `d + 0` is a LONGLONG, and unary plus
        // does not convert at all. Measured, not derived.
        (
            "SELECT -d, +d, -dt3 FROM E",
            vec!["Double", "Date", "Double"],
        ),
        // A string operand makes it a double, as it does for any other value
        // with no numeric type of its own.
        ("SELECT d + s, d + '1' FROM E", vec!["Double"; 2]),
        // A YEAR is already a number, so it is a LONGLONG and not a decimal.
        ("SELECT y + 0, y * 1 FROM E", vec!["LongLong"; 2]),
        // The aggregates, the casts and the bitwise operators.
        (
            "SELECT SUM(d), AVG(d), MAX(d), MIN(tm) FROM E",
            vec!["NewDecimal", "NewDecimal", "Date", "Time"],
        ),
        (
            "SELECT CAST(d AS SIGNED), CAST(d AS DECIMAL(20,2)), CAST(d AS CHAR), d | 0 FROM E",
            vec!["LongLong", "NewDecimal", "VarString", "LongLong"],
        ),
        // A precision written into the statement counts as much as one on a
        // column: `NOW(3)` and a cast both make the operand a decimal.
        (
            "SELECT NOW(3) + 0, NOW() + 0, CAST('2024-01-05 06:07:08.5' AS DATETIME(1)) + 0",
            vec!["NewDecimal", "LongLong", "NewDecimal"],
        ),
    ] {
        assert_eq!(codes(&engine, &session, sql), expected, "{sql}");
    }

    // A YEAR is UNSIGNED on the wire, which `information_schema` does not say
    // — it reports plain `year` — and which the arithmetic over one keeps.
    let unsigned = |sql: &str| -> Vec<bool> {
        engine
            .describe(&session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| c.to_wire().flags & mysql_ultralight_protocol::column::flags::UNSIGNED != 0)
            .collect()
    };
    assert_eq!(
        unsigned("SELECT y, y + 0, y * 1, -y, d FROM E"),
        vec![true, true, true, false, false]
    );
}

#[test]
fn an_enum_in_a_numeric_context_is_its_ordinal() {
    // MySQL gives an ENUM a number: the 1-based position it was declared at.
    // This engine stored the member as text and read the number out of those
    // characters, so every member that is not itself a number answered 0.
    // Measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE E (id INT NOT NULL, e ENUM('a','b','c'), n ENUM('7','8','9'), \
         r ENUM('b','a'), s VARCHAR(10), PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO E VALUES (1, 'a', '7', 'b', 'a'), (2, 'c', '9', 'a', 'c')",
    );
    for (sql, expected) in [
        ("SELECT e + 0 FROM E WHERE id = 1", "1"),
        ("SELECT e + 0 FROM E WHERE id = 2", "3"),
        ("SELECT -e FROM E WHERE id = 1", "-1"),
        ("SELECT e = 1 FROM E WHERE id = 1", "1"),
        ("SELECT CAST(e AS SIGNED) FROM E WHERE id = 1", "1"),
        ("SELECT e = 'a' FROM E WHERE id = 1", "1"),
        // The case that proves the ordinal is not the text: `n` holds the
        // characters `7` and its ordinal is 1, so `n + 0` is 1 and `n = 7` is
        // false while `n = '7'` is true.
        ("SELECT n + 0 FROM E WHERE id = 1", "1"),
        ("SELECT n = 1 FROM E WHERE id = 1", "1"),
        ("SELECT n = 7 FROM E WHERE id = 1", "0"),
        ("SELECT n = '7' FROM E WHERE id = 1", "1"),
        ("SELECT n + 0 FROM E WHERE id = 2", "3"),
        // The counterweight: the same characters in a VARCHAR really are read
        // by their leading number, which is what the tag exists to separate.
        ("SELECT s + 0 FROM E WHERE id = 1", "0"),
        ("SELECT 'a' + 0", "0"),
        // Every other numeric reading goes through the same ordinal.
        ("SELECT e * 2 FROM E WHERE id = 2", "6"),
        ("SELECT e DIV 1 FROM E WHERE id = 2", "3"),
        ("SELECT e MOD 2 FROM E WHERE id = 2", "1"),
        ("SELECT e | 0 FROM E WHERE id = 2", "3"),
        ("SELECT e & 1 FROM E WHERE id = 2", "1"),
        (
            "SELECT CAST(e AS DECIMAL(10,2)) FROM E WHERE id = 2",
            "3.00",
        ),
        // A string context is the member, which is the distinction itself.
        ("SELECT CAST(e AS CHAR) FROM E WHERE id = 2", "c"),
        ("SELECT CONCAT(e, '') FROM E WHERE id = 2", "c"),
        ("SELECT JSON_ARRAY(e) FROM E WHERE id = 2", "[\"c\"]"),
        // A comparison against a number is numeric and one against a string is
        // not, in the same column.
        ("SELECT e IN (1, 2) FROM E WHERE id = 1", "1"),
        ("SELECT e IN (1, 2) FROM E WHERE id = 2", "0"),
        ("SELECT e IN ('a', 'b') FROM E WHERE id = 1", "1"),
        ("SELECT e BETWEEN 1 AND 2 FROM E WHERE id = 1", "1"),
        ("SELECT e = 'zzz' FROM E WHERE id = 1", "0"),
        // Aggregates fold the ordinals.
        ("SELECT SUM(e) FROM E", "4"),
        ("SELECT AVG(e) FROM E", "2"),
        ("SELECT SUM(n) FROM E", "4"),
        // Truthiness asks the same number, which is the reading that made
        // `WHERE e` skip every row whose member is not a digit.
        ("SELECT IF(e, 'y', 'n') FROM E WHERE id = 1", "y"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
    // NULL stays NULL through all of it rather than becoming the error member.
    run(&engine, &mut session, "INSERT INTO E (id) VALUES (3)");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT e, e + 0, e = 1 FROM E WHERE id = 3"
        ),
        vec![vec!["NULL", "NULL", "NULL"]]
    );
}

#[test]
fn an_enum_sorts_by_its_ordinal_and_compares_by_its_member() {
    // Two answers from one value, and MySQL does not give the same one to
    // both: `ORDER BY r` over `ENUM('b','a')` puts `b` first — the declared
    // order — while `r1 < r2` over that same type answers by the members, so
    // `'b' < 'a'` is false where the ordinals 1 and 2 would make it true.
    // `MIN` and `MAX` go with the comparison. All measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE R (id INT NOT NULL, r ENUM('b','a'), r2 ENUM('b','a'), PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO R VALUES (1, 'b', 'a'), (2, 'a', 'b')",
    );
    for (sql, expected) in [
        // The sort: declared order, not the alphabet.
        ("SELECT r FROM R ORDER BY r, id", vec![vec!["b"], vec!["a"]]),
        (
            "SELECT r FROM R ORDER BY r DESC, id",
            vec![vec!["a"], vec!["b"]],
        ),
        (
            "SELECT DISTINCT r FROM R ORDER BY r",
            vec![vec!["b"], vec!["a"]],
        ),
        // Through a GROUP BY as well, where the ORDER BY names an output
        // column: resolving that alias through a rendered literal dropped the
        // ordinal and sorted this the other way round.
        (
            "SELECT r, COUNT(*) FROM R GROUP BY r ORDER BY r",
            vec![vec!["b", "1"], vec!["a", "1"]],
        ),
        // The comparison: by the member, so it disagrees with the sort above.
        ("SELECT r < r2 FROM R WHERE id = 1", vec![vec!["0"]]),
        ("SELECT r > r2 FROM R WHERE id = 1", vec![vec!["1"]]),
        ("SELECT r > 'a' FROM R WHERE id = 1", vec![vec!["1"]]),
        ("SELECT r < 'a' FROM R WHERE id = 1", vec![vec!["0"]]),
        // MIN and MAX side with the comparison, and hand the winner back
        // without its ordinal — `MIN(r) + 0` is 0 in MySQL where `r + 0` is
        // the position.
        ("SELECT MIN(r) FROM R", vec![vec!["a"]]),
        ("SELECT MAX(r) FROM R", vec![vec!["b"]]),
        ("SELECT MIN(r) + 0 FROM R", vec![vec!["0"]]),
        // HAVING and an ORDER BY *expression* both name the projected value,
        // which reaches them as a hidden column rather than as a literal
        // spelling. Written as a quoted member instead, `HAVING r = 1`
        // compared `'b'` against the number and returned no group at all,
        // while the `SELECT` beside it read the ordinal — one statement
        // answering the same question two ways.
        (
            "SELECT r, COUNT(*) FROM R GROUP BY r HAVING r = 1",
            vec![vec!["b", "1"]],
        ),
        (
            "SELECT r, COUNT(*) FROM R GROUP BY r HAVING r + 0 = 1",
            vec![vec!["b", "1"]],
        ),
        (
            "SELECT r, COUNT(*) FROM R GROUP BY r ORDER BY r + 0 DESC",
            vec![vec!["a", "1"], vec!["b", "1"]],
        ),
    ] {
        assert_eq!(rows(&engine, &mut session, sql), expected, "{sql}");
    }
}

#[test]
fn an_enum_column_can_hold_and_copy_the_error_member() {
    // Index 0 is MySQL's error member, the empty string, and a column really
    // does hold it: `'0'` writes one. It has to survive being read back out
    // and written again — `UPDATE t SET g = g` and `INSERT ... SELECT` both
    // keep it on 8.3, including into a column declared with different members
    // — while `g = ''` stays 1265 for a column that declares no empty member.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Z (id INT NOT NULL, g ENUM('a','b','c'), PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE Z2 (id INT NOT NULL, g ENUM('c','b','a'), PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Z VALUES (1, '0'), (2, 'a')",
    );
    run(&engine, &mut session, "UPDATE Z SET g = g");
    assert_eq!(
        rows(&engine, &mut session, "SELECT g, g + 0 FROM Z ORDER BY id"),
        vec![vec!["", "0"], vec!["a", "1"]]
    );
    // Into a different declaration the member is resolved again, so `a`
    // becomes 3 — and the error member stays the error member.
    run(&engine, &mut session, "INSERT INTO Z2 SELECT id, g FROM Z");
    assert_eq!(
        rows(&engine, &mut session, "SELECT g, g + 0 FROM Z2 ORDER BY id"),
        vec![vec!["", "0"], vec!["a", "3"]]
    );
}

#[test]
fn an_inexact_total_reads_each_value_the_way_a_row_does() {
    // `SUM` and `AVG` over a string read each value by its leading number,
    // which is the same rule `t + 0` reads a row by: `'12abc'` and `'3.5x'`
    // total 15.5 on 8.3, and a BLOB is read exactly as the TEXT beside it is.
    // Dropping whatever would not parse as a whole number answered 0 for both,
    // so one statement disagreed with itself about the same two values — and a
    // binary string, which has no whole-string parse at all, always did.
    //
    // With nothing numeric in the column the answer is 0 and not `-0`: Rust's
    // `Sum for f64` folds from `-0.0`, and the average shows it too because
    // `-0.0` over a count is still `-0.0`.
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE S (t TEXT, b BLOB)");
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES ('12abc', '12abc'), ('3.5x', '3.5x')",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT SUM(t), AVG(t), SUM(b), AVG(b) FROM S"
        ),
        vec![vec!["15.5", "7.75", "15.5", "7.75"]]
    );
    // The row beside the total, which is the half that has to agree with it.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT t + 0, b + 0 FROM S ORDER BY t"
        ),
        vec![vec!["12", "12"], vec!["3.5", "3.5"]]
    );
    run(&engine, &mut session, "CREATE TABLE Z (t TEXT)");
    run(&engine, &mut session, "INSERT INTO Z VALUES ('x'), ('y')");
    assert_eq!(
        rows(&engine, &mut session, "SELECT SUM(t), AVG(t) FROM Z"),
        vec![vec!["0", "0"]]
    );
}

#[test]
fn a_temporal_under_an_approximate_code_is_sent_as_one() {
    // `DATE_ADD` over a VARCHAR is typed a *string* by MySQL and computed as a
    // double — `SUM` of it is DOUBLE 20260103060708.5 — while the engine packs
    // the temporal and totals it exactly. The value and the code then
    // disagreed, and the two protocols cannot both send that: the text one
    // writes every declared digit and the binary one parses them into an f64.
    //
    // The DATETIME(6) column beside it is the counterweight: there the type
    // really is exact, and the extra digits stay.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL, v VARCHAR(40), dt6 DATETIME(6), PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES (1, '2026-01-02 06:07:08.500000', '2026-01-02 06:07:08.500000')",
    );
    let sql = "SELECT SUM(DATE_ADD(v, INTERVAL 1 DAY)), SUM(DATE_ADD(dt6, INTERVAL 1 DAY)) FROM T";
    assert_eq!(
        rows(&engine, &mut session, sql),
        vec![vec!["20260103060708.5", "20260103060708.500000"]]
    );
    let codes: Vec<String> = engine
        .describe(&session, sql, 0)
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| format!("{:?}", c.ty))
        .collect();
    assert_eq!(codes, vec!["Double", "NewDecimal"]);
}

#[test]
fn a_set_operation_does_not_carry_an_enum_through_it() {
    // MySQL materialises a set operation, and an ENUM does not survive one:
    // the column comes out VAR_STRING, `x + 0` over it is 0 and the order is
    // by the member. Measured with the *same* declaration on both sides as
    // well as with two different ones, so it is the materialising and not a
    // mixture of types that does it.
    //
    // The mixed pair is also the only way two ordinals meaning different
    // members reached one sort key: position 1 is `b` in one column and `a` in
    // the other, and ordering by them compared numbers that are not
    // comparable.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE E (id INT NOT NULL, e ENUM('a','b','c'), r ENUM('b','a'),          PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO E VALUES (1,'a','b'), (2,'c','a')",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT x FROM (SELECT r AS x FROM E UNION ALL SELECT e AS x FROM E) d ORDER BY x"
        ),
        vec![vec!["a"], vec!["a"], vec!["b"], vec!["c"]]
    );
    let sql = "SELECT x, x + 0 FROM (SELECT r AS x FROM E UNION ALL SELECT r AS x FROM E) d                ORDER BY x";
    assert_eq!(
        rows(&engine, &mut session, sql),
        vec![
            vec!["a", "0"],
            vec!["a", "0"],
            vec!["b", "0"],
            vec!["b", "0"]
        ]
    );
    // The column is described as a plain string too, not as the ENUM either
    // branch declared — asserted on the bare set operation, because a derived
    // table's outer projection re-derives the type from the relation and would
    // hide what the operation itself reported. MySQL says VAR_STRING for both
    // shapes and for both the mixed and the matching pair.
    let codes = |sql: &str| -> Vec<String> {
        engine
            .describe(&session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| format!("{:?}", c.ty))
            .collect()
    };
    assert_eq!(
        codes("SELECT r FROM E UNION ALL SELECT e FROM E"),
        vec!["VarString"]
    );
    assert_eq!(
        codes("SELECT r FROM E UNION ALL SELECT r FROM E"),
        vec!["VarString"]
    );
    // And the bare column beside it keeps the ENUM code it was declared with.
    assert_eq!(codes("SELECT r FROM E"), vec!["String"]);
    assert_eq!(codes(sql), vec!["VarString", "Double"]);
}

#[test]
fn an_enum_keeps_its_ordinal_through_some_functions_and_not_others() {
    // Which is measured rather than derived, because the two lists do not
    // follow from anything: `IF`, `CASE` and `NULLIF` hand the ENUM through
    // and keep the ordinal, while `COALESCE`, `IFNULL`, `GREATEST`, `LEAST`
    // and a scalar subquery build a string result and lose it. `COALESCE(e)`
    // has one argument and nothing to aggregate against, and still loses it.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE F (id INT NOT NULL, e ENUM('a','b','c'), PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO F VALUES (1, 'c')");
    for (sql, expected) in [
        ("SELECT e + 0 FROM F", "3"),
        ("SELECT IF(1, e, 'z') + 0 FROM F", "3"),
        ("SELECT (CASE WHEN 1 THEN e END) + 0 FROM F", "3"),
        ("SELECT NULLIF(e, 'z') + 0 FROM F", "3"),
        ("SELECT COALESCE(e) + 0 FROM F", "0"),
        ("SELECT COALESCE(e, 'z') + 0 FROM F", "0"),
        ("SELECT IFNULL(e, 'z') + 0 FROM F", "0"),
        ("SELECT GREATEST(e, e) + 0 FROM F", "0"),
        ("SELECT LEAST(e, e) + 0 FROM F", "0"),
        ("SELECT (SELECT e FROM F WHERE id = 1) + 0", "0"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn an_enum_expression_is_typed_the_way_mysql_types_it() {
    // The value and the type are decided in different code for the same
    // expression, and the binary protocol encodes by the *type* — so an
    // ordinal under a STRING code reaches a client as the member's characters.
    // Every code here measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE E (e ENUM('a','b','c'), t TEXT, b BLOB, j JSON, c CHAR(4))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE I (id INT NOT NULL, n INT, PRIMARY KEY (id))",
    );
    let codes = |sql: &str| -> Vec<String> {
        engine
            .describe(&session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| format!("{:?}", c.ty))
            .collect()
    };
    for (sql, expected) in [
        // The column itself is a STRING, as MySQL reports it — the ENUM flag
        // that goes with it there is not set by this engine.
        ("SELECT e FROM E", "String"),
        // An ENUM has no numeric type of its own, so it converts to a double
        // and everything computed from it is approximate. `e + 0.0` is a
        // DOUBLE and not the NEWDECIMAL two exact operands would give.
        ("SELECT e + 0 FROM E", "Double"),
        ("SELECT e + 0.0 FROM E", "Double"),
        ("SELECT -e FROM E", "Double"),
        ("SELECT SUM(e) FROM E", "Double"),
        ("SELECT AVG(e) FROM E", "Double"),
        // The same is true of the string types beside it, which is how the
        // ENUM gets MySQL's answer without a rule of its own — measured
        // across the family rather than assumed from the type that prompted
        // it. A VARCHAR is missing on purpose: its code is what
        // `computed_column_type` also answers for an expression it cannot
        // type, so it stays exact here and diverges from MySQL, which is a
        // gap this change does not close.
        ("SELECT t + 0.0 FROM E", "Double"),
        ("SELECT SUM(b) FROM E", "Double"),
        ("SELECT -j FROM E", "Double"),
        ("SELECT SUM(c) FROM E", "Double"),
        // A cast says what it wants, and MIN keeps what it folded.
        ("SELECT CAST(e AS SIGNED) FROM E", "LongLong"),
        ("SELECT MIN(e) FROM E", "String"),
        // The counterweight to the rule above: `VAR_STRING` is also the
        // answer for an expression this cannot type at all, so it must stay
        // *out* of the string family. MySQL types this NEWDECIMAL, and the
        // evaluator still produces an integer for it — announcing DOUBLE
        // would have the binary protocol send eight bytes of a float.
        (
            "SELECT SUM((SELECT n FROM I WHERE id = 1)) FROM I",
            "NewDecimal",
        ),
    ] {
        assert_eq!(codes(sql), vec![expected.to_owned()], "{sql}");
    }

    // What a client actually reads, which is a second rendering: `rows` above
    // reads the engine's and the protocol has its own. The ordinal is a
    // reading and not a representation, so the wire carries the member — a
    // column described STRING that sent `3` would be a number under a string
    // code, which the binary protocol cannot even encode.
    run(
        &engine,
        &mut session,
        "INSERT INTO E VALUES ('c', 't', 'b', '1', 'c')",
    );
    let Outcome::Rows(result) = run(&engine, &mut session, "SELECT e, e + 0 FROM E") else {
        panic!("expected rows")
    };
    let encoded = |value: &Value| {
        let mut text = mysql_ultralight_protocol::BufWriter::new();
        value.to_wire().encode_text(&mut text);
        // A length-encoded string: one byte of length, then the bytes.
        String::from_utf8_lossy(&text.into_inner()[1..]).into_owned()
    };
    assert_eq!(encoded(&result.rows[0][0]), "c");
    assert_eq!(encoded(&result.rows[0][1]), "3");
}

#[test]
fn an_enum_is_assigned_by_member_first_and_by_index_second() {
    // The write side of the same fact. A member wins over an index, so `'2'`
    // into `ENUM('2','1')` is the *first* member; a number is always an index,
    // so `2` into that same column is the second. Index 0 is MySQL's error
    // member, the empty string, and only a string reaches it — the number 0 is
    // refused. All measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE A (id INT NOT NULL, g ENUM('a','b','c'), d ENUM('2','1'), PRIMARY KEY (id))",
    );
    for (id, sql, member, ordinal) in [
        (1, "INSERT INTO A (id, g) VALUES (1, 2)", "b", "2"),
        (2, "INSERT INTO A (id, g) VALUES (2, '2')", "b", "2"),
        // Case-insensitively, and stored in the spelling the column declared.
        (3, "INSERT INTO A (id, g) VALUES (3, 'A')", "a", "1"),
        // A number truncates towards zero rather than rounding, so 1.6 is
        // still the first member.
        (4, "INSERT INTO A (id, g) VALUES (4, 1.6)", "a", "1"),
        // The error member, reachable only through a string.
        (5, "INSERT INTO A (id, g) VALUES (5, '0')", "", "0"),
    ] {
        run(&engine, &mut session, sql);
        assert_eq!(
            rows(
                &engine,
                &mut session,
                &format!("SELECT g, g + 0 FROM A WHERE id = {id}")
            ),
            vec![vec![member, ordinal]],
            "{sql}"
        );
    }
    // A member wins over an index, which is only visible where the two would
    // disagree: `'2'` is `d`'s first member and its second is `1`.
    run(
        &engine,
        &mut session,
        "INSERT INTO A (id, d) VALUES (6, '2')",
    );
    run(&engine, &mut session, "INSERT INTO A (id, d) VALUES (7, 2)");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT d, d + 0 FROM A WHERE id IN (6, 7) ORDER BY id"
        ),
        vec![vec!["2", "1"], vec!["1", "2"]]
    );
    // Everything that is neither a member nor an index in range is 1265, and
    // that includes the number 0 and a string that is not a whole number.
    for sql in [
        "INSERT INTO A (id, g) VALUES (8, 0)",
        "INSERT INTO A (id, g) VALUES (8, 9)",
        "INSERT INTO A (id, g) VALUES (8, -1)",
        "INSERT INTO A (id, g) VALUES (8, 'zzz')",
        "INSERT INTO A (id, g) VALUES (8, '1.0')",
        "INSERT INTO A (id, g) VALUES (8, '')",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1265, "{sql}");
    }
    // NULL is not an assignment to convert at all.
    run(
        &engine,
        &mut session,
        "INSERT INTO A (id, g) VALUES (9, NULL)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT g, g + 0 FROM A WHERE id = 9"),
        vec![vec!["NULL", "NULL"]]
    );
}

#[test]
fn a_column_filled_from_its_implicit_default_holds_an_enum() {
    // The value an `INSERT IGNORE` writes into a NOT NULL column it was not
    // given. It goes into the row without passing `coerce`, so it has to carry
    // the ordinal on its own — otherwise it is the one member in the column
    // that reads 0 in arithmetic while every other row reads its position.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (id INT NOT NULL, e ENUM('a','b') NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT IGNORE INTO D (id) VALUES (1)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT e, e + 0 FROM D"),
        vec![vec!["a", "1"]]
    );
}

#[test]
fn a_column_filled_from_its_implicit_default_holds_bytes() {
    // The value a backfill writes into a NOT NULL binary column. It goes into
    // the row without passing `coerce`, so it has to carry the variant on its
    // own — and everything that reads a binary column reads the variant, not
    // the column's declaration, so a row that held text was a binary column
    // answering two ways.
    //
    // A bitwise operator is what shows it, because it is the one consumer that
    // *refuses* a binary operand rather than converting it: with the default
    // left as text, `b & 1` answered 0 for the backfilled row and raised 1235
    // for a row inserted normally, from the same column.
    let (engine, mut session) = engine();
    for sql in [
        "CREATE TABLE D (id INT NOT NULL, PRIMARY KEY (id))",
        "INSERT INTO D (id) VALUES (1)",
        "ALTER TABLE D ADD COLUMN b VARBINARY(8) NOT NULL",
        "INSERT INTO D (id, b) VALUES (2, 'x')",
    ] {
        run(&engine, &mut session, sql);
    }

    for id in [1, 2] {
        let sql = format!("SELECT b & 1 FROM D WHERE id = {id}");
        let err = engine.execute(&mut session, &sql, &[]).unwrap_err();
        assert_eq!(err.code, 1235, "{sql}");
    }

    // And it is still the empty string everywhere that reads one, so the
    // variant is the only thing that changed.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT b, SOUNDEX(b) FROM D ORDER BY id"
        ),
        vec![vec!["", ""], vec!["x", "X000"]]
    );
}

#[test]
fn a_column_filled_from_its_implicit_default_holds_a_temporal() {
    // The value an `INSERT IGNORE` writes into a NOT NULL column it was not
    // given. It goes into the row without passing `coerce`, so it has to carry
    // the tag on its own — otherwise it is the one DATE in the column that
    // reads 1970 in arithmetic while every other row reads 19700101.
    //
    // The value itself is the epoch rather than MySQL's `0000-00-00`, because
    // the mode this engine targets sets NO_ZERO_DATE and would reject that —
    // which is why this is asserted here and not asked of the differential.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (id INT NOT NULL, d DATE NOT NULL, dt DATETIME NOT NULL, \
         tm TIME NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT IGNORE INTO D (id) VALUES (1)",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT d + 0, dt + 0, tm + 0 FROM D"),
        vec![vec!["19700101", "19700101000000", "0"]]
    );
}

#[test]
fn a_temporal_is_stored_in_the_shape_its_type_declares() {
    // MySQL stores a temporal by its *type*, not by how it was written, and
    // this engine kept the characters: a DATETIME given a bare date holds
    // midnight, a DATE given a datetime keeps only the date, and a fraction is
    // rounded to the declared precision. Keeping the text made the packed
    // number a property of the statement rather than of the column.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE S (id INT NOT NULL, dt DATETIME, d DATE, tm TIME, tm1 TIME(1), \
         dt6 DATETIME(6), roll TIME, edge DATETIME, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO S VALUES (1, '2024-01-05', '2024-01-05 06:07:08', '06:07:08.567', \
         '06:07:08.567', '2024-01-05 06:07:08.567', '23:59:59.567', \
         '2024-01-05 23:59:59.567')",
    );
    for (sql, expected) in [
        ("SELECT dt FROM S", "2024-01-05 00:00:00"),
        ("SELECT dt + 0 FROM S", "20240105000000"),
        ("SELECT d FROM S", "2024-01-05"),
        ("SELECT d + 0 FROM S", "20240105"),
        // A DATE written with a clock on it still equals its own date, which
        // it did not while the clock was kept.
        ("SELECT d = '2024-01-05' FROM S", "1"),
        // A TIME with no declared precision rounds the fraction away.
        ("SELECT tm FROM S", "06:07:09"),
        ("SELECT tm + 0 FROM S", "60709"),
        ("SELECT tm1 FROM S", "06:07:08.6"),
        ("SELECT tm1 + 0 FROM S", "60708.6"),
        ("SELECT dt6 FROM S", "2024-01-05 06:07:08.567000"),
        // The rounding carries, and a TIME has no day to carry into where a
        // DATETIME does.
        ("SELECT roll FROM S", "24:00:00"),
        ("SELECT edge FROM S", "2024-01-06 00:00:00"),
        // A cast declares a shape and a precision exactly as a column does.
        ("SELECT CAST('06:07:08.567' AS TIME)", "06:07:09"),
        ("SELECT CAST('06:07:08.567' AS TIME(1))", "06:07:08.6"),
        ("SELECT CAST('06:07:08.567' AS TIME(1)) + 0", "60708.6"),
        ("SELECT CAST(dt6 AS DATETIME) FROM S", "2024-01-05 06:07:09"),
        ("SELECT CAST(dt6 AS DATETIME) + 0 FROM S", "20240105060709"),
        (
            "SELECT CAST('2024-01-05' AS DATETIME)",
            "2024-01-05 00:00:00",
        ),
        ("SELECT CAST('2024-01-05 06:07:08' AS DATE)", "2024-01-05"),
        // A shift keeps the precision of its argument's declared type, which a
        // string literal does not have — so a fraction written into one is
        // padded to the full six digits where a DATETIME(6) column is not.
        (
            "SELECT DATE_ADD(dt6, INTERVAL 1 DAY) FROM S",
            "2024-01-06 06:07:08.567000",
        ),
        ("SELECT DATE_ADD(tm1, INTERVAL 1 SECOND) FROM S", "NULL"),
        (
            "SELECT DATE_ADD('2026-01-01 00:00:00.1', INTERVAL 1 SECOND)",
            "2026-01-01 00:00:01.100000",
        ),
        (
            "SELECT DATE_ADD('2026-01-01', INTERVAL 1 SECOND)",
            "2026-01-01 00:00:01",
        ),
        (
            "SELECT DATE_ADD(dt, INTERVAL 1 SECOND) FROM S",
            "2024-01-05 00:00:01",
        ),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn a_fraction_never_lands_under_an_integer_column() {
    // The worst shape this engine has: the two protocols disagreeing. A value
    // carrying a fraction under a column described LONGLONG is sent as its
    // digits by the text protocol and as *zero* by the binary one, because
    // `encode_binary` parses it as an `i64` and the parse fails. Same row, two
    // connections, two answers, and nothing in a test that reads the text
    // would ever see it.
    //
    // So the invariant is asserted directly rather than through a rendering:
    // under an integer code the value has to be an integer.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE F (id INT NOT NULL, tm TIME, dt DATETIME, dt3 DATETIME(3), \
         PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO F VALUES (1, '06:07:08.567', '2024-01-05 06:07:08.567', \
         '2024-01-05 06:07:08.567')",
    );
    for sql in [
        "SELECT tm + 0 FROM F",
        "SELECT dt + 0 FROM F",
        "SELECT CAST('06:07:08.567' AS TIME) + 0",
        "SELECT CAST(dt3 AS DATETIME) + 0 FROM F",
        "SELECT DATE_ADD(dt3, INTERVAL 1 DAY) + 0 FROM F",
        "SELECT COALESCE(dt3, dt3) + 0 FROM F",
        "SELECT MAX(dt3) + 0 FROM F",
        "SELECT tm * 1, dt - 0, tm MOD 100 FROM F",
    ] {
        let Outcome::Rows(result) = run(&engine, &mut session, sql) else {
            panic!("expected rows from: {sql}");
        };
        for (index, column) in result.columns.iter().enumerate() {
            let integral = matches!(
                column.ty,
                mysql_ultralight_protocol::column::ColumnType::Tiny
                    | mysql_ultralight_protocol::column::ColumnType::Short
                    | mysql_ultralight_protocol::column::ColumnType::Int24
                    | mysql_ultralight_protocol::column::ColumnType::Long
                    | mysql_ultralight_protocol::column::ColumnType::LongLong
                    | mysql_ultralight_protocol::column::ColumnType::Year
            );
            for row in &result.rows {
                let value = &row[index];
                assert!(
                    !integral || value.is_null() || matches!(value, Value::Int(_)),
                    "{sql} column {index} is {:?} under {:?}",
                    value,
                    column.ty
                );
            }
        }
    }

    // And the values themselves, so the invariant cannot be met by answering
    // the wrong integer.
    for (sql, expected) in [
        ("SELECT tm + 0 FROM F", "60709"),
        ("SELECT dt + 0 FROM F", "20240105060709"),
        ("SELECT CAST('06:07:08.567' AS TIME) + 0", "60709"),
        (
            "SELECT DATE_ADD(dt3, INTERVAL 1 DAY) + 0 FROM F",
            "20240106060708.567",
        ),
        ("SELECT MAX(dt3) + 0 FROM F", "20240105060708.567"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn an_exponent_reaches_a_decimal_as_digits_and_not_through_a_float() {
    // A `DECIMAL(65,30)` asks for thirty places and an `f64` holds about
    // fifteen digits in total, so folding the value through one wrote noise
    // into every place past the fifteenth: `1e3` came back
    // 1000.000000000000089690419062898688 where MySQL answers thirty zeros.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE X (id INT NOT NULL, wide DECIMAL(65,30), two DECIMAL(20,2), \
         PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO X VALUES (1, '1e3', '1e3')",
    );
    for (sql, expected) in [
        ("SELECT wide FROM X", "1000.000000000000000000000000000000"),
        ("SELECT two FROM X", "1000.00"),
        (
            "SELECT CAST('1e3' AS DECIMAL(65,30))",
            "1000.000000000000000000000000000000",
        ),
        ("SELECT CAST('1e3' AS DECIMAL(20,2))", "1000.00"),
        ("SELECT CAST('1.5e2' AS DECIMAL(10,3))", "150.000"),
        ("SELECT CAST('-2.5e-1' AS DECIMAL(10,4))", "-0.2500"),
        // Text that is not a number at all still reads as the number MySQL
        // reads out of it, which is the fallback this shares.
        ("SELECT CAST('abc' AS DECIMAL(20,2))", "0.00"),
        ("SELECT CAST('12abc' AS DECIMAL(20,2))", "12.00"),
        ("SELECT CAST('1e' AS DECIMAL(20,2))", "1.00"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn the_clock_functions_read_their_precision_argument() {
    // The argument of `CURTIME(3)` is a fractional-second precision, and
    // ignoring it is not cosmetic: the metadata reads the same token to type
    // the column, so the column was described a decimal and answered an
    // integer. `NOW(3)` already read it; `CURTIME` did not.
    //
    // The value moves, so its width is what is asserted.
    let (engine, mut session) = engine();
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT LENGTH(CURTIME(3)), LENGTH(CURTIME()), LENGTH(NOW(3)), LENGTH(NOW())"
        ),
        vec![vec!["12", "8", "23", "19"]]
    );
    // And they are NOT NULL whatever precision they were asked for: `NOW(NULL)`
    // is a syntax error in MySQL, so there is no value to pass in that could
    // make one. The argument-count rule beside this was measured against
    // `UNIX_TIMESTAMP`, where it does hold.
    let nullable = |sql: &str| -> Vec<bool> {
        engine
            .describe(&session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| c.to_wire().flags & mysql_ultralight_protocol::column::flags::NOT_NULL == 0)
            .collect()
    };
    assert_eq!(
        nullable("SELECT NOW(3), CURTIME(3), NOW(), UNIX_TIMESTAMP(), UNIX_TIMESTAMP(NULL)"),
        vec![false, false, false, false, true]
    );
}

#[test]
fn a_temporal_expression_converts_like_a_temporal_column() {
    // The tag rides on the value rather than being read off the statement,
    // which is measured and not obvious: MySQL types
    // `DATE_ADD('2024-01-05', INTERVAL 1 DAY)` a *string* — `STRING`, not
    // `DATE` — and still reads it 20240106, where the bare literal beside it
    // reads 2024. A conversion driven by the declared type would say 2024.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (d DATE, dt DATETIME)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES ('2024-01-05', '2024-01-05 06:07:08')",
    );
    for (sql, expected) in [
        (
            "SELECT DATE_ADD('2024-01-05', INTERVAL 1 DAY) + 0",
            "20240106",
        ),
        ("SELECT DATE_ADD(d, INTERVAL 1 DAY) + 0 FROM T", "20240106"),
        ("SELECT ADDDATE(d, 1) + 0 FROM T", "20240106"),
        ("SELECT DATE_SUB(d, INTERVAL 1 DAY) + 0 FROM T", "20240104"),
        ("SELECT TIMESTAMPADD(DAY, 1, d) + 0 FROM T", "20240106"),
        ("SELECT DATE('2024-01-05 06:07:08') + 0", "20240105"),
        ("SELECT DATE(dt) + 0 FROM T", "20240105"),
        ("SELECT LAST_DAY('2024-01-05') + 0", "20240131"),
        ("SELECT CAST('2024-01-05' AS DATE) + 0", "20240105"),
        ("SELECT CAST('06:07:08' AS TIME) + 0", "60708"),
        // The same expressions unconverted, so a test that lost the tag on
        // both sides could not pass by agreeing with itself.
        ("SELECT '2024-01-05' + 0", "2024"),
        ("SELECT CURDATE() + 0 > 20200101", "1"),
        ("SELECT NOW() + 0 > 20200101000000", "1"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn coalesce_makes_a_temporal_a_string_where_if_and_greatest_do_not() {
    // Two families that look alike and are not, measured rather than reasoned
    // from each other: `COALESCE` and `IFNULL` aggregate their arguments'
    // types, so anything non-temporal in them makes the answer a string and it
    // reads 2024. `IF`, `GREATEST` and `CASE` hand back the argument itself
    // and keep the temporal, which reads 20240105.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (d DATE, dt DATETIME)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES ('2024-01-05', '2024-01-05 06:07:08')",
    );
    for (sql, expected) in [
        ("SELECT COALESCE(d, '0') + 0 FROM T", "2024"),
        ("SELECT IFNULL(d, '0') + 0 FROM T", "2024"),
        // A number beside it does the same thing: MySQL types the whole call
        // VAR_STRING, not a number.
        ("SELECT COALESCE(d, 0) + 0 FROM T", "2024"),
        ("SELECT COALESCE(d, 1.5) + 0 FROM T", "2024"),
        // With nothing but temporals in it the tag stays, and a NULL argument
        // does not count as something beside it.
        ("SELECT COALESCE(d, d) + 0 FROM T", "20240105"),
        ("SELECT COALESCE(d, dt) + 0 FROM T", "20240105"),
        ("SELECT COALESCE(NULL, d) + 0 FROM T", "20240105"),
        ("SELECT IFNULL(d, NULL) + 0 FROM T", "20240105"),
        ("SELECT IFNULL(NULL, d) + 0 FROM T", "20240105"),
        ("SELECT IF(1, d, '0') + 0 FROM T", "20240105"),
        ("SELECT GREATEST(d, '0') + 0 FROM T", "20240105"),
        (
            "SELECT (CASE WHEN 1 THEN d ELSE '0' END) + 0 FROM T",
            "20240105",
        ),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn a_bitwise_temporal_operand_is_its_packed_digits() {
    // MySQL converts a temporal *type* to its packed digits here — a DATE of
    // 2026-01-02 is 20260102, a DATETIME 20260102030405, a TIME of 06:07:08 is
    // 60708 — and a *string* of the same characters by its leading integer,
    // giving 2026 and 6. Both measured on 8.3.
    //
    // Until #0054 the engine stored a temporal as `Value::Text`, so the two
    // were one value by the time an operator saw them and this refused rather
    // than picking. `Value::Temporal` is what tells them apart, and every
    // shape below that used to refuse now answers.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INT NOT NULL, d DATE, dt DATETIME, tm TIME, \
         ts TIMESTAMP NULL, y YEAR, s VARCHAR(30), near VARCHAR(30), part VARCHAR(30), \
         packed VARCHAR(30), noisy VARCHAR(30), clockish VARCHAR(30), \
         dtm DATETIME)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES (1, '2026-01-02', '2026-01-02 03:04:05', '06:07:08', \
         '2026-01-02 03:04:05', 2026, '2026-01-02', '1-2', '2026-01', '20260102', '2026-01-02 x', '12:34', '2026-01-02 03:04')",
    );
    for (sql, expected) in [
        ("SELECT d | 0 FROM T", "20260102"),
        ("SELECT dt | 0 FROM T", "20260102030405"),
        ("SELECT tm | 0 FROM T", "60708"),
        ("SELECT ts | 0 FROM T", "20260102030405"),
        ("SELECT CAST('2026-01-02' AS DATE) | 0", "20260102"),
        // `IF`, `GREATEST` and a `CASE` keep the temporal in MySQL — all
        // three are 20260102 there — where `COALESCE` and `IFNULL` turn it
        // into a string as soon as anything beside it is one. Measured, not
        // reasoned from each other: the two families disagree.
        ("SELECT IF(1, d, '0') | 0 FROM T", "20260102"),
        ("SELECT GREATEST(d, '0') | 0 FROM T", "20260102"),
        (
            "SELECT (CASE WHEN 1 THEN d ELSE '0' END) | 0 FROM T",
            "20260102",
        ),
        ("SELECT COALESCE(d, d) | 0 FROM T", "20260102"),
        ("SELECT IFNULL(d, NULL) | 0 FROM T", "20260102"),
        ("SELECT COALESCE(d, '0') | 0 FROM T", "2026"),
        ("SELECT IFNULL(d, '0') | 0 FROM T", "2026"),
        ("SELECT CAST('06:07:08' AS TIME) | 0", "60708"),
        // The fraction packs after the point and is then rounded like any
        // other decimal operand, which is MySQL's answer for both of these:
        // 06:07:08.25 rounds down to 60708 and 03:04:05.5 up to ...406.
        ("SELECT CAST('06:07:08.25' AS TIME) | 0", "60708"),
        (
            "SELECT CAST('2026-01-02 03:04:05.5' AS DATETIME(1)) | 0",
            "20260102030406",
        ),
        // A DATETIME stored with its seconds left off. The engine keeps the
        // text it was written with rather than normalising it, and the packing
        // widens the missing group — which is what MySQL reads that column as.
        ("SELECT dtm | 0 FROM T", "20260102030400"),
        // The counterweight, and the reason the tag had to exist: a VARCHAR
        // holding the same characters is a string, and MySQL reads it 2026.
        // This is the case a shape rule over the text could never get right,
        // because there is nothing in `'2026-01-02'` to tell the two apart.
        ("SELECT s | 0 FROM T", "2026"),
        ("SELECT '2026-01-02' | 0", "2026"),
        ("SELECT '2026-01-02 03:04:05' | 0", "2026"),
        ("SELECT '06:07:08' | 0", "6"),
        ("SELECT '2026' | 0", "2026"),
        ("SELECT '20260102' | 0", "20260102"),
        // A YEAR is an integer and never had the problem.
        ("SELECT y | 0 FROM T", "2026"),
        ("SELECT near | 0 FROM T", "1"),
        ("SELECT part | 0 FROM T", "2026"),
        ("SELECT packed | 0 FROM T", "20260102"),
        ("SELECT noisy | 0 FROM T", "2026"),
        ("SELECT clockish | 0 FROM T", "12"),
        // An expression the statement declares a string reads as one, which
        // it always did — these are 2026 in MySQL over the DATE column.
        ("SELECT CAST(d AS CHAR) | 0 FROM T", "2026"),
        ("SELECT CONCAT(d, '') | 0 FROM T", "2026"),
        ("SELECT LEFT(d, 10) | 0 FROM T", "2026"),
        ("SELECT SUBSTRING(d, 1, 10) | 0 FROM T", "2026"),
        ("SELECT TRIM(d) | 0 FROM T", "2026"),
        ("SELECT UPPER(d) | 0 FROM T", "2026"),
        ("SELECT CAST('2026-01-02' AS CHAR) | 0", "2026"),
        ("SELECT CONCAT('2026-', '01-02') | 0", "2026"),
        ("SELECT LEFT('2026-01-02 03:04:05', 10) | 0", "2026"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // `~d` answers since #0069 gave `Value` an unsigned tag. It refused before
    // that with 1235, because 18446744073689291513 is above `i64::MAX` and had
    // nothing to be — which was #0077 and never this: the operand had been
    // converting correctly the whole time, and this is the proof of it.
    assert_eq!(
        rows(&engine, &mut session, "SELECT ~d FROM T"),
        vec![vec!["18446744073689291513"]]
    );
}

#[test]
fn the_bitwise_operators_bind_the_way_mysql_binds_them() {
    // sqlparser numbers these operators from Postgres: it puts `^` below both
    // `+` and `*`, and `&` above `<<`. MySQL binds `^` tightest of the set,
    // then `* / DIV %`, then `+ -`, then `<< >>`, then `&`, then `|` — read
    // off 8.3 rather than off the manual, and checked against a reference
    // evaluator over 14000 random expressions. The comment on each line is
    // what sqlparser's grouping answers instead; a line without one is a case
    // the two already agree on, kept so a fix that overshoots shows up.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT 2 ^ 3 + 1", "2"),       // 2 ^ (3 + 1) = 6
        ("SELECT 1 + 2 ^ 3", "2"),       // (1 + 2) ^ 3 = 0
        ("SELECT 2 ^ 3 * 2", "2"),       // 2 ^ (3 * 2) = 4
        ("SELECT 2 ^ 3 % 3", "1"),       // 2 ^ (3 % 3) = 2
        ("SELECT 2 ^ 3 DIV 4", "0"),     // 2 ^ (3 DIV 4) = 2
        ("SELECT 1 & 2 ^ 3", "1"),       // (1 & 2) ^ 3 = 3
        ("SELECT 1 | 2 ^ 3", "1"),       // (1 | 2) ^ 3 = 0
        ("SELECT 1 << 2 ^ 3", "2"),      // (1 << 2) ^ 3 = 7
        ("SELECT 1 << 2 & 3", "0"),      // 1 << (2 & 3) = 4
        ("SELECT 2 & 4 >> 1", "2"),      // (2 & 4) >> 1 = 0
        ("SELECT 4 >> 1 & 2", "2"),      // 4 >> (1 & 2) = 4
        ("SELECT 1 + 2 * 3 ^ 4", "15"),  // (1 + 2 * 3) ^ 4 = 3
        ("SELECT 1 & 2 ^ 3 + 4", "1"),   // (1 & 2) ^ (3 + 4) = 7
        ("SELECT 7 DIV 2 ^ 3 + 1", "8"), // 7 DIV (2 ^ (3 + 1)) = 1
        // A tighter operator either side of a looser one. One rotation cannot
        // reach these: the decision needs the left operand's *final* root, and
        // `2 ^ 3 + 2` is what the left of this becomes only after it has been
        // corrected itself. A fuzz of 6000 expressions found ~150 such shapes,
        // all of them silently wrong.
        ("SELECT 2 ^ 3 + 2 ^ 3", "2"),   // (2 ^ (3 + 2)) ^ 3 = 0
        ("SELECT 7 ^ 1 + 6 ^ 7", "7"),   // (7 ^ (1 + 6)) ^ 7 = 11
        ("SELECT 1 + 2 ^ 3 ^ 7", "7"),   // ((1 + 2) ^ 3) ^ 7 = 5
        ("SELECT 1 << 3 & 7 << 3", "8"), // ((1 << 3) & 7) << 3 = 0
        ("SELECT 3 ^ 3 & 4 ^ 2", "0"),   // ((3 ^ (3 & 4)) ^ 2) = 2
        ("SELECT 6 & 5 >> 1 << 3", "0"), // ((6 & 5) >> 1) << 3 = 16
        ("SELECT 1 ^ 2 & 3", "3"),
        ("SELECT 1 ^ 2 | 3", "3"),
        ("SELECT 1 & 2 << 3", "0"),
        ("SELECT 1 + 2 << 3", "24"),
        ("SELECT 1 << 2 + 3", "32"),
        ("SELECT 1 * 2 << 3", "16"),
        ("SELECT 1 << 2 * 3", "64"),
        ("SELECT 1 & 2 | 3", "3"),
        ("SELECT 1 | 2 & 3", "3"),
        ("SELECT 1 >> 2 | 3", "3"),
        // Left-associative, which the rebuild has to be told: every one of
        // these means something else read right to left.
        ("SELECT 2 >> 1 << 3", "8"), // 2 >> (1 << 3) = 0
        ("SELECT 8 >> 1 >> 1", "2"), // 8 >> (1 >> 1) = 8
        ("SELECT 1 - 2 - 3", "-4"),  // 1 - (2 - 3) = 2
        // Parentheses are their own node, so nothing regroups through them.
        ("SELECT (1 + 2) ^ 3", "0"),
        ("SELECT 1 + (2 ^ 3)", "2"),
        // `~` is a prefix and binds tighter than every operator, where
        // sqlparser reads its operand at the precedence of `+` and so lets it
        // swallow a `*`, a `DIV` or a `%`.
        ("SELECT ~-3 * 2", "4"),        // ~((-3) * 2) = 5
        ("SELECT ~-9 DIV 2", "4"),      // ~((-9) DIV 2) = 3
        ("SELECT ~-9 % 3", "2"),        // ~((-9) % 3) = 18446744073709551615
        ("SELECT 3 ^ ~(-6) * 2", "12"), // 3 ^ ~((-6) * 2) = 8
        // Nested, which needs the *outer* one corrected before the inner one
        // is: correcting the inner first hands `~1` to a multiplication, and
        // `~1` is a value the engine refuses.
        ("SELECT ~ ~ 1 * 7", "7"), // ~(~(1 * 7)) = 7, by a different route
        ("SELECT ~ ~ 3 * 5 % 9", "6"),
        // And on the *left* of a chain, where the walk never looks: the walk
        // only ever reads the right operand, so the rebuild is the only thing
        // that can see this `~` has the `* 2` inside it.
        ("SELECT ~(-6) * 2 ^ 3", "5"),   // ~((-6) * 2) ^ 3 = 8
        ("SELECT ~(-9) DIV 2 ^ 3", "8"), // ~((-9) DIV 2) ^ 3 = 11
        ("SELECT 1 + ~(-6) * 2", "11"),
        ("SELECT 8 - ~(-4) DIV 2", "7"),
        ("SELECT ~-3 * 2 + 1", "5"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // The corrected shape is what the column is typed from, as it is for
    // `DIV`: `2 ^ 3 / 2` reaches the engine with `^` at its root and is really
    // a division, which MySQL reports as a nullable, signed decimal. Reading
    // the root as parsed promised NOT NULL and UNSIGNED for a column holding
    // 0.5.
    let described = engine
        .describe(&session, "SELECT 2 ^ 3 / 2, 2 ^ 3 + 1", 0)
        .unwrap()
        .unwrap();
    let shape: Vec<String> = described
        .iter()
        .map(|c| format!("{:?} {} {}", c.ty, c.unsigned, c.nullable))
        .collect();
    assert_eq!(shape, vec!["NewDecimal false true", "LongLong true false"]);
}

#[test]
fn arithmetic_reads_a_number_out_of_anything_it_is_given() {
    // MySQL's arithmetic has no "not a number" case — it reads the leading
    // numeric part and calls the rest zero. Answering NULL instead made every
    // one of these NULL, which is a wrong answer rather than a missing one.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT 'x' + 1", "1"),
        ("SELECT 'x' * 2", "0"),
        ("SELECT 'x' - 1", "-1"),
        ("SELECT '' + 1", "1"),
        ("SELECT '12abc' + 1", "13"),
        ("SELECT '-3abc' * 2", "-6"),
        ("SELECT 'x' DIV 2", "0"),
        // An exponent counts when it is complete and not otherwise, which is
        // the one part the leading-number reader used to stop short of.
        ("SELECT '1e2' + 1", "101"),
        ("SELECT '2e-1' + 0", "0.2"),
        ("SELECT '1e' + 1", "2"),
        ("SELECT '.5' + 1", "1.5"),
        ("SELECT ' 5 ' + 1", "6"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // NULL is still NULL: it is not a value with a numeric reading.
    assert_eq!(
        rows(&engine, &mut session, "SELECT NULL + 1, 1 + NULL"),
        vec![vec!["NULL", "NULL"]]
    );

    // Reading the exponent made overflow reachable where stopping at the `e`
    // never could. MySQL stops a string conversion at DBL_MAX, and an infinity
    // renders as the token `Inf`, which is not a number a client can read back.
    // This asserted the leading digits only, because the whole rendering used
    // to be 309 of them where MySQL writes an exponent — it can say what MySQL
    // says now.
    assert_eq!(
        rows(&engine, &mut session, "SELECT '1e400' + 0, '-1e400' + 0"),
        vec![vec!["1.7976931348623157e308", "-1.7976931348623157e308"]]
    );
}

#[test]
fn a_type_synonym_is_the_type_it_is_a_synonym_for() {
    // Each of these is a spelling MySQL accepts for a type it already has, and
    // each was either the wrong type or no type at all. Measured on 8.3.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Syn (
            r REAL, r4 FLOAT4, ru REAL UNSIGNED, dp DOUBLE PRECISION,
            dpu DOUBLE PRECISION UNSIGNED, du DEC(10,2) UNSIGNED,
            i8u BIGINT UNSIGNED, b BINARY, quoted SET('''a''','b')
        )",
    );

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, COLUMN_TYPE, CHARACTER_MAXIMUM_LENGTH \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'Syn' \
             ORDER BY ORDINAL_POSITION"
        ),
        vec![
            // REAL is a synonym for DOUBLE, not for FLOAT. Only FLOAT4 is the
            // narrow one, and the two shared an arm.
            vec!["r", "double", "NULL"],
            vec!["r4", "float", "NULL"],
            vec!["ru", "double unsigned", "NULL"],
            vec!["dp", "double", "NULL"],
            vec!["dpu", "double unsigned", "NULL"],
            vec!["du", "decimal(10,2) unsigned", "NULL"],
            vec!["i8u", "bigint unsigned", "NULL"],
            // A bare BINARY is BINARY(1), and reported 65535 as its length.
            vec!["b", "binary(1)", "1"],
            // The parser hands members over already unquoted, so trimming
            // quotes off them only ever reached a member made of quotes.
            vec!["quoted", "set('''a''','b')", "5"],
        ]
    );
}

#[test]
fn an_unsigned_decimal_refuses_a_negative() {
    // Accepting `UNSIGNED` on a DECIMAL or a DOUBLE without enforcing it would
    // store a value MySQL rejects — a wrong answer rather than a gap, which is
    // the one outcome this engine may not produce.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Money (d DECIMAL(6,2) UNSIGNED, f DOUBLE UNSIGNED)",
    );
    run(&engine, &mut session, "INSERT INTO Money VALUES (1.5, 1.5)");

    for (sql, column) in [
        ("INSERT INTO Money VALUES (-1.5, 1.5)", "d"),
        ("INSERT INTO Money VALUES (1.5, -1.5)", "f"),
    ] {
        let error = engine
            .execute(&mut session, sql, &[])
            .expect_err("a negative belongs in neither column");
        assert_eq!(error.code, 1264, "{sql}");
        assert!(error.message.contains(column), "{sql}: {}", error.message);
    }

    // Negative zero is not below zero, and MySQL takes it — reading the sign
    // character alone turned a value it stores into an error.
    run(
        &engine,
        &mut session,
        "INSERT INTO Money VALUES (-0.00, -0.0)",
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
        .unwrap()
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
            "SELECT doc + 0, doc * 2, CAST(doc AS SIGNED), doc = txt, doc = 3, doc = '3' \
             FROM Num"
        ),
        // Read as a number it is 3, but *compared* it is the JSON number 3,
        // and a string operand beside a document is read as a JSON string —
        // so it equals the number 3 and not the text '3'. This asserted that
        // `doc = txt` was true, from before the comparison was a comparison of
        // documents; MySQL 8.3 says 0.
        vec![vec!["3", "6", "3", "0", "1", "0"]]
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
    // The two real ones have to stay unimplemented for this to mean anything:
    // it read `SOUNDEX` until soundex was implemented, and then failed for
    // being out of date rather than for finding something.
    for sql in [
        "SELECT NOT_A_FUNCTION(NULL)",
        "SELECT SHA1(NULL)",
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
    let described = engine
        .describe(&session, "SELECT * FROM W", 0)
        .unwrap()
        .unwrap();
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
        .unwrap()
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
    let described = engine
        .describe(&session, "SELECT iu FROM Q", 0)
        .unwrap()
        .unwrap();
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
        format!(
            "{:?}",
            engine.describe(session, sql, 0).unwrap().unwrap()[0].ty
        )
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
        .unwrap()
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
        // ABS and ROUND keep the kind but not the width: every integer
        // argument answers LONGLONG however narrow, because the answer can
        // outgrow it. GREATEST and LEAST are the other way round — they hand
        // back an argument unchanged, so its own width is the answer.
        (
            "SELECT ABS(i), ROUND(i), GREATEST(i, i), LEAST(i, i) FROM E",
            vec!["LongLong", "LongLong", "Long", "Long"],
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

#[test]
fn a_sql_boolean_beside_a_document_is_a_boolean() {
    // MySQL reads the keyword TRUE as the JSON value `true` and the literal 1
    // as the JSON number 1, so the two compare against different documents.
    // The engine had no boolean value, so both evaluated to 1 and the first
    // row below answered 0. The whole of #0061. Measured on 8.3.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT CAST('true' AS JSON) = TRUE", "1"),
        ("SELECT CAST('true' AS JSON) = 1", "0"),
        ("SELECT CAST('1' AS JSON) = TRUE", "0"),
        ("SELECT CAST('1' AS JSON) = 1", "1"),
        // FALSE and 0 split the same way, so the tag is not a special case
        // for the true half.
        ("SELECT CAST('false' AS JSON) = FALSE", "1"),
        ("SELECT CAST('false' AS JSON) = 0", "0"),
        ("SELECT CAST('0' AS JSON) = FALSE", "0"),
        ("SELECT CAST('0' AS JSON) = 0", "1"),
        // Either way round, and through the NULL-safe spelling and IN.
        ("SELECT TRUE = CAST('true' AS JSON)", "1"),
        ("SELECT 1 = CAST('true' AS JSON)", "0"),
        ("SELECT CAST('true' AS JSON) <=> TRUE", "1"),
        ("SELECT CAST('true' AS JSON) <> TRUE", "0"),
        ("SELECT CAST('true' AS JSON) IN (TRUE)", "1"),
        ("SELECT CAST('true' AS JSON) IN (1)", "0"),
        // JSON's type order puts a boolean above a number, so an inequality
        // against a document is not merely "not equal": it has a direction.
        ("SELECT CAST('1' AS JSON) < TRUE", "1"),
        ("SELECT CAST('true' AS JSON) < TRUE", "0"),
        ("SELECT CAST('true' AS JSON) > TRUE", "0"),
        // A conversion, not just a comparison: MySQL builds the JSON boolean
        // where the literal 1 builds the number.
        ("SELECT CAST(TRUE AS JSON)", "true"),
        ("SELECT CAST(FALSE AS JSON)", "false"),
        ("SELECT JSON_TYPE(CAST(TRUE AS JSON))", "BOOLEAN"),
        ("SELECT JSON_TYPE(CAST(1 AS JSON))", "INTEGER"),
        // Inside a document a boolean stays one, which is the same tag seen
        // from the constructing side rather than the comparing side.
        ("SELECT JSON_ARRAY(TRUE)", "[true]"),
        ("SELECT JSON_ARRAY(FALSE)", "[false]"),
        ("SELECT JSON_ARRAY(1)", "[1]"),
        ("SELECT JSON_OBJECT('a', TRUE)", "{\"a\": true}"),
        (
            "SELECT JSON_ARRAY(TRUE, FALSE, 1, 0)",
            "[true, false, 1, 0]",
        ),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn every_yes_or_no_answer_is_a_boolean() {
    // The keyword is the smaller half of the surface. Every comparison,
    // logical operator, postfix test and predicate function is typed boolean
    // by MySQL, so each of these is `[true]` on 8.3 and not `[1]` — which is
    // why the tag is applied where all of them already met rather than at the
    // literal alone.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT JSON_ARRAY(1 = 1)", "[true]"),
        ("SELECT JSON_ARRAY(1 < 2)", "[true]"),
        ("SELECT JSON_ARRAY(1 <=> 1)", "[true]"),
        ("SELECT JSON_ARRAY(1 <> 1)", "[false]"),
        ("SELECT JSON_ARRAY(1 IN (1, 2))", "[true]"),
        ("SELECT JSON_ARRAY(1 NOT IN (2))", "[true]"),
        ("SELECT JSON_ARRAY(1 BETWEEN 0 AND 2)", "[true]"),
        ("SELECT JSON_ARRAY('a' LIKE 'a')", "[true]"),
        ("SELECT JSON_ARRAY('a' NOT LIKE 'b')", "[true]"),
        ("SELECT JSON_ARRAY(1 IS NULL)", "[false]"),
        ("SELECT JSON_ARRAY(1 IS NOT NULL)", "[true]"),
        ("SELECT JSON_ARRAY(1 IS TRUE)", "[true]"),
        ("SELECT JSON_ARRAY(TRUE IS NOT FALSE)", "[true]"),
        ("SELECT JSON_ARRAY(NOT 0)", "[true]"),
        ("SELECT JSON_ARRAY(1 AND 1)", "[true]"),
        ("SELECT JSON_ARRAY(1 OR 0)", "[true]"),
        ("SELECT JSON_ARRAY(TRUE AND FALSE)", "[false]"),
        ("SELECT JSON_ARRAY(EXISTS(SELECT 1))", "[true]"),
        ("SELECT JSON_ARRAY(ISNULL(1))", "[false]"),
        ("SELECT JSON_ARRAY(JSON_CONTAINS('[1]', '1'))", "[true]"),
        ("SELECT JSON_ARRAY(JSON_VALID('1'))", "[true]"),
        ("SELECT JSON_ARRAY(REGEXP_LIKE('a', 'a'))", "[true]"),
        // NULL is not false. A three-valued answer stays NULL rather than
        // becoming the JSON boolean `false`.
        ("SELECT JSON_ARRAY(NOT NULL)", "[null]"),
        ("SELECT JSON_ARRAY(TRUE OR NULL)", "[true]"),
        ("SELECT JSON_ARRAY(NULL AND 0)", "[false]"),
        // The counterweight: a function that answers a *number* which happens
        // to be 1 or 2 is not boolean, so the tag is not simply "anything
        // small".
        ("SELECT JSON_ARRAY(LOCATE('a', 'ba'))", "[2]"),
        ("SELECT JSON_ARRAY(CHAR_LENGTH('a'))", "[1]"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
    // The comparison against a document is the same tag from the other side,
    // so a bare comparison compares as `true` where the literal 1 does not.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST('true' AS JSON) = (1 = 1), CAST('true' AS JSON) = (1 AND 1)"
        ),
        vec![vec!["1", "1"]]
    );

    // And each predicate is described the integer it answers. Asserted at the
    // metadata's own level because every case above wraps the call in
    // `JSON_ARRAY`, which exercises the value and hides the code — which is
    // how `REGEXP_LIKE` stayed VAR_STRING while answering 0 or 1.
    assert_eq!(
        engine
            .describe(
                &session,
                "SELECT REGEXP_LIKE('a', 'a'), ISNULL(1), JSON_VALID('1'), \
                 JSON_CONTAINS('[1]', '1')",
                0
            )
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| format!("{:?}", c.ty))
            .collect::<Vec<_>>(),
        vec!["LongLong"; 4]
    );
    // And `REGEXP_LIKE` is NOT NULL when its arguments are — the other half of
    // the same gap the type code above was. It follows its arguments rather
    // than being unconditional, because `REGEXP_LIKE(NULL, 'a')` is NULL.
    let nullable = |sql: &str| -> Vec<bool> {
        engine
            .describe(&session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| c.nullable)
            .collect()
    };
    assert_eq!(nullable("SELECT REGEXP_LIKE('a', 'a')"), vec![false]);
    assert_eq!(nullable("SELECT REGEXP_LIKE(NULL, 'a')"), vec![true]);
}

#[test]
fn a_boolean_reads_as_one_or_zero_everywhere_but_a_document() {
    // The tag changes the *document* reading and nothing else, which is what
    // keeps it from being a second integer type. Every answer here is the one
    // the untagged `Int(0|1)` already gave, and each was measured on 8.3
    // rather than assumed — `TRUE + 0` is a LONGLONG there, not a double.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT TRUE", "1"),
        ("SELECT FALSE", "0"),
        ("SELECT TRUE = 1", "1"),
        ("SELECT TRUE + 0", "1"),
        ("SELECT -TRUE", "-1"),
        ("SELECT TRUE + TRUE", "2"),
        ("SELECT CAST(TRUE AS CHAR)", "1"),
        ("SELECT CAST(TRUE AS SIGNED)", "1"),
        ("SELECT CONCAT(TRUE, '')", "1"),
        ("SELECT SUM(TRUE)", "1"),
        ("SELECT MIN(TRUE)", "1"),
        ("SELECT TRUE IS TRUE", "1"),
        ("SELECT 1 IS TRUE", "1"),
        ("SELECT TRUE DIV 1", "1"),
        ("SELECT TRUE MOD 2", "1"),
        // A comparison against anything that is not a document is numeric,
        // so `'abc'` is 0 rather than sorting beside the character `1`.
        ("SELECT '1' = TRUE", "1"),
        ("SELECT 1.0 = TRUE", "1"),
        ("SELECT 'abc' = TRUE", "0"),
        // The spellings that separate the numeric reading from the collated
        // one: `'1.0'` and `'01'` are the number 1 and are not the characters
        // `1`, so a boolean that compared by its text would answer 0 here.
        ("SELECT '1.0' = TRUE", "1"),
        ("SELECT '01' = TRUE", "1"),
        ("SELECT '0.0' = FALSE", "1"),
        ("SELECT TRUE LIKE '1'", "1"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
    // Arithmetic over a boolean stays exact. Invisible in `SELECT`, where a
    // double renders `1` too, and not invisible in a document — the reason
    // this is asserted through JSON_ARRAY as well as through the column type.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(TRUE + 0), JSON_ARRAY(-TRUE), JSON_ARRAY(TRUE + TRUE), \
             JSON_ARRAY(TRUE & 1), JSON_ARRAY(TRUE << 1), JSON_ARRAY(SUM(TRUE)) \
             FROM (SELECT 1) t"
        ),
        vec![vec!["[1]", "[-1]", "[2]", "[1]", "[2]", "[1]"]]
    );
    // The numeric functions too. `ABS` is the only one of these with an
    // integer fast path of its own — the rest build their answer through
    // `whole` — so it is the only one a boolean could fall out of, and it did.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(ABS(TRUE)), JSON_ARRAY(ROUND(TRUE)), JSON_ARRAY(FLOOR(TRUE)), \
             JSON_ARRAY(CEIL(TRUE)), JSON_ARRAY(TRUNCATE(TRUE, 0)), JSON_ARRAY(MOD(TRUE, 2))"
        ),
        vec![vec!["[1]", "[1]", "[1]", "[1]", "[1]", "[1]"]]
    );
    // And it is announced as an integer, so a binary client decodes eight
    // bytes rather than a string: the tag never crosses the wire. Asserted at
    // the metadata's own level rather than through a row, because the two are
    // decided in different code and a value that disagreed with its code is
    // exactly the split this would otherwise miss.
    let codes = |sql: &str| -> Vec<String> {
        engine
            .describe(&session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| format!("{:?}", c.ty))
            .collect()
    };
    assert_eq!(
        codes("SELECT TRUE, FALSE, 1 = 1, TRUE + 0, -TRUE, NOT 0, 1 <=> 1"),
        vec!["LongLong"; 7]
    );
    // `<=>` was described VAR_STRING and nullable while answering an integer,
    // which putting it in the corpus for #0061 found: it is the one
    // comparison that answers whatever it is given, so it is NOT NULL over
    // two NULLs.
    assert!(
        !engine
            .describe(&session, "SELECT NULL <=> NULL", 0)
            .unwrap()
            .unwrap()[0]
            .nullable
    );
    // And the bytes themselves, because a wire value is where a tag that only
    // *reads* as an integer would finally show: a `Str` under a LONGLONG code
    // desynchronises a binary client.
    assert_eq!(
        Value::Bool(true).to_wire(),
        mysql_ultralight_protocol::Value::I64(1)
    );
    assert_eq!(
        Value::Bool(false).to_wire(),
        mysql_ultralight_protocol::Value::I64(0)
    );
}

#[test]
fn a_boolean_does_not_survive_being_put_anywhere() {
    // Measured on 8.3, and it is *not* the list an ENUM follows — the two were
    // measured separately and disagree in both directions. `IF`, `CASE` and
    // `NULLIF` keep an ENUM's ordinal and drop a boolean; a scalar subquery
    // drops the ordinal and keeps the boolean.
    let (engine, mut session) = engine();
    for (sql, expected) in [
        ("SELECT JSON_ARRAY(IF(1, TRUE, FALSE))", "[1]"),
        // Both branches, because the tag is taken off where the value is
        // chosen and one arm handling it would read exactly like two.
        ("SELECT JSON_ARRAY(IF(0, 1, TRUE))", "[1]"),
        (
            "SELECT JSON_ARRAY(CASE WHEN 1 THEN TRUE ELSE FALSE END)",
            "[1]",
        ),
        ("SELECT JSON_ARRAY(CASE WHEN 0 THEN 1 ELSE TRUE END)", "[1]"),
        ("SELECT JSON_ARRAY(NULLIF(TRUE, 0))", "[1]"),
        // With one argument, so it is the function and not a mixture of types
        // that takes the tag off.
        ("SELECT JSON_ARRAY(COALESCE(TRUE))", "[1]"),
        ("SELECT JSON_ARRAY(COALESCE(NULL, TRUE))", "[1]"),
        ("SELECT JSON_ARRAY(IFNULL(TRUE, 0))", "[1]"),
        ("SELECT JSON_ARRAY(IFNULL(NULL, TRUE))", "[1]"),
        ("SELECT JSON_ARRAY(GREATEST(TRUE, FALSE))", "[1]"),
        ("SELECT JSON_ARRAY(GREATEST(FALSE, TRUE))", "[1]"),
        ("SELECT JSON_ARRAY(LEAST(TRUE, FALSE))", "[0]"),
        ("SELECT JSON_ARRAY(LEAST(TRUE, TRUE))", "[1]"),
        // Every aggregate, including the two that hand a value straight
        // through.
        // Through a one-row FROM, because an aggregate wrapped in a call
        // needs one to be planned as an aggregate at all.
        ("SELECT JSON_ARRAY(MIN(TRUE)) FROM (SELECT 1) t", "[1]"),
        ("SELECT JSON_ARRAY(MAX(TRUE)) FROM (SELECT 1) t", "[1]"),
        ("SELECT JSON_ARRAY(SUM(TRUE)) FROM (SELECT 1) t", "[1]"),
        ("SELECT JSON_ARRAY(COUNT(TRUE)) FROM (SELECT 1) t", "[1]"),
        // Anything that gives the value a column to live in.
        ("SELECT JSON_ARRAY(x) FROM (SELECT TRUE AS x) t", "[1]"),
        (
            "WITH c AS (SELECT TRUE AS x) SELECT JSON_ARRAY(x) FROM c",
            "[1]",
        ),
        (
            "SELECT JSON_ARRAY(x) FROM (SELECT TRUE AS x UNION ALL SELECT TRUE) t LIMIT 1",
            "[1]",
        ),
        // A set operation on its own, reached through a scalar subquery so
        // the answer is visible at all. The subquery keeps the tag and the
        // union takes it off, which is what makes this the case that separates
        // the two — the derived table above would have hidden either one.
        ("SELECT JSON_ARRAY((SELECT TRUE UNION SELECT TRUE))", "[1]"),
        (
            "SELECT JSON_ARRAY((SELECT TRUE UNION ALL SELECT TRUE LIMIT 1))",
            "[1]",
        ),
        // And the counterweights, which are what stop the rule being read as
        // "the tag never survives anything": a scalar subquery, a
        // parenthesised expression and a unary plus all keep it.
        ("SELECT JSON_ARRAY((SELECT TRUE))", "[true]"),
        ("SELECT JSON_ARRAY((TRUE))", "[true]"),
        ("SELECT JSON_ARRAY(+TRUE)", "[true]"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
    // A derived table is not a scalar subquery even when it is inside one, so
    // the tag is gone by the time the outer query sees it.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT (SELECT TRUE) = CAST('true' AS JSON), \
             (SELECT x FROM (SELECT TRUE AS x) t) = CAST('true' AS JSON), \
             (SELECT TRUE UNION SELECT TRUE) = CAST('true' AS JSON), \
             CAST('true' AS JSON) IN (SELECT TRUE), \
             CAST('true' AS JSON) IN (SELECT TRUE UNION SELECT TRUE)"
        ),
        vec![vec!["1", "0", "0", "1", "0"]]
    );
    // A window function is one too, positional or aggregate. The positional
    // ones read their subject straight out of a row where the aggregates go
    // through the fold, so they were the one family that answered two ways —
    // `FIRST_VALUE(TRUE)` matched a document of `true` where `MAX(TRUE)`
    // beside it did not. Measured on 8.3: every column here is 0.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Win (id INT NOT NULL, PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO Win VALUES (1), (2)");
    // Read back through a scalar subquery, which is the wrapper that *keeps*
    // the tag — so anything still tagged here would answer 1, and every
    // column is 0 on 8.3. `LAG`'s third argument is in because the default is
    // taken from a literal rather than from a row and untags separately.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT CAST('true' AS JSON) = (SELECT FIRST_VALUE(TRUE) OVER () FROM Win LIMIT 1), \
             CAST('true' AS JSON) = (SELECT LAST_VALUE(TRUE) OVER () FROM Win LIMIT 1), \
             CAST('true' AS JSON) = (SELECT LEAD(TRUE) OVER (ORDER BY id) FROM Win LIMIT 1), \
             CAST('true' AS JSON) = (SELECT LAG(1, 1, TRUE) OVER (ORDER BY id) FROM Win LIMIT 1), \
             CAST('true' AS JSON) = (SELECT MAX(TRUE) OVER () FROM Win LIMIT 1)"
        ),
        vec![vec!["0", "0", "0", "0", "0"]]
    );
    // A table value constructor is table-shaped too, and it is the one whose
    // *type* the tag reached: `wire_type` reads the first row, a boolean is
    // not a `Value::Int`, and the column was described VAR_STRING while
    // carrying 1. MySQL types `column_0` LONGLONG.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT column_0 = CAST('true' AS JSON) FROM (VALUES ROW(TRUE)) v"
        ),
        vec![vec!["0"]]
    );

    // A user variable is somewhere to put a value, so it holds 1 and not the
    // keyword that was assigned to it.
    run(&engine, &mut session, "SET @x = TRUE");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(@x), CAST('true' AS JSON) = @x"
        ),
        vec![vec!["[1]", "0"]]
    );

    // The constructor's *type* as well as its value, because that is the half
    // the tag actually reached: the column is derived from the first row, so
    // an untagged row is what keeps a boolean and the literal 1 describing the
    // same column. MySQL says LONGLONG for both.
    let codes = |sql: &str| -> Vec<String> {
        engine
            .describe(&session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| format!("{:?}", c.ty))
            .collect()
    };
    assert_eq!(codes("VALUES ROW(TRUE)"), codes("VALUES ROW(1)"));
    assert_eq!(codes("VALUES ROW(TRUE)"), vec!["LongLong"]);
    // Its nullability came from nothing at all and was announced nullable
    // whatever it held. From the expressions, and NOT NULL only when *every*
    // row's is, since the rows share one column.
    let nullable = |sql: &str| -> Vec<bool> {
        engine
            .describe(&session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| c.nullable)
            .collect()
    };
    assert_eq!(nullable("VALUES ROW(TRUE), ROW(FALSE)"), vec![false]);
    assert_eq!(nullable("VALUES ROW(1), ROW(0)"), vec![false]);
    assert_eq!(nullable("VALUES ROW(TRUE), ROW(NULL)"), vec![true]);
}

#[test]
fn a_column_never_holds_a_boolean() {
    // The crux, and the reason the state image does not have to learn a tag:
    // MySQL makes BOOLEAN a TINYINT(1), and a column holding 1 is the *number*
    // 1 however it was written. `j = f` below is 0 on 8.3 for a row whose
    // document is `true` and whose column holds 1, where `j = TRUE` is 1.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE B (id INT NOT NULL, f BOOLEAN, t TINYINT(1), n INT, \
         v VARCHAR(10), g ENUM('2','1'), j JSON, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO B VALUES (1, TRUE, 1, 1, TRUE, TRUE, CAST('true' AS JSON)), \
         (2, FALSE, 0, 0, FALSE, '1', CAST('false' AS JSON))",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, j = f, j = t, j = n, j = TRUE, j = 1 FROM B ORDER BY id"
        ),
        vec![
            vec!["1", "0", "0", "0", "1", "0"],
            vec!["2", "0", "0", "0", "0", "0"],
        ]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(f), JSON_ARRAY(t), JSON_ARRAY(n), JSON_ARRAY(v) \
             FROM B WHERE id = 1"
        ),
        vec![vec!["[1]", "[1]", "[1]", "[\"1\"]"]]
    );
    // Assignment reads the keyword as the *number* it is, which is the one
    // place a column can tell 1 from '1': `TRUE` into `ENUM('2','1')` is the
    // first member, exactly as the literal 1 is, where the text `'1'` matches
    // the member spelled `1`.
    assert_eq!(
        rows(&engine, &mut session, "SELECT g FROM B ORDER BY id"),
        vec![vec!["2"], vec!["1"]]
    );
}

/// A table holding one row of every temporal shape `EXTRACT` reads.
fn extractable(engine: &Engine, session: &mut Session) {
    run(
        engine,
        session,
        "CREATE TABLE X (d DATE NULL, dt DATETIME NULL, dt6 DATETIME(6) NULL, \
         tm TIME NULL, tmneg TIME NULL, tm6 TIME(6) NULL, ts TIMESTAMP NULL, \
         y YEAR NULL, s VARCHAR(40) NULL)",
    );
    run(
        engine,
        session,
        "INSERT INTO X VALUES ('2024-01-05', '2024-01-05 06:07:08', \
         '2024-01-05 06:07:08.123456', '06:07:08', '-06:07:08', '-06:07:08.123456', \
         '2024-01-05 06:07:08', 2024, '2024-01-05 06:07:08')",
    );
}

#[test]
fn extract_answers_every_unit_mysql_takes_from_it() {
    // Every number here was read off MySQL 8.3 rather than worked out. The
    // column list is the point: the same unit answers differently for a DATE,
    // a DATETIME, a TIME and a TIMESTAMP, and a clock unit over a DATE is 0
    // where a calendar unit over a TIME is a day of *this* month.
    let (engine, mut session) = engine();
    extractable(&engine, &mut session);
    for (sql, expected) in [
        (
            "SELECT EXTRACT(YEAR FROM d), EXTRACT(QUARTER FROM d), EXTRACT(MONTH FROM d), \
             EXTRACT(WEEK FROM d), EXTRACT(DAY FROM d) FROM X",
            vec!["2024", "1", "1", "0", "5"],
        ),
        (
            "SELECT EXTRACT(HOUR FROM d), EXTRACT(MINUTE FROM d), EXTRACT(SECOND FROM d), \
             EXTRACT(MICROSECOND FROM d) FROM X",
            vec!["0", "0", "0", "0"],
        ),
        (
            "SELECT EXTRACT(HOUR FROM dt), EXTRACT(MINUTE FROM dt), EXTRACT(SECOND FROM dt), \
             EXTRACT(MICROSECOND FROM dt) FROM X",
            vec!["6", "7", "8", "0"],
        ),
        (
            "SELECT EXTRACT(SECOND FROM dt6), EXTRACT(MICROSECOND FROM dt6), \
             EXTRACT(DAY FROM dt6) FROM X",
            vec!["8", "123456", "5"],
        ),
        (
            "SELECT EXTRACT(HOUR FROM ts), EXTRACT(DAY FROM ts), EXTRACT(YEAR FROM ts) FROM X",
            vec!["6", "5", "2024"],
        ),
        (
            "SELECT EXTRACT(HOUR FROM tm), EXTRACT(MINUTE FROM tm), EXTRACT(SECOND FROM tm) \
             FROM X",
            vec!["6", "7", "8"],
        ),
        (
            "SELECT EXTRACT(MICROSECOND FROM tm6), EXTRACT(SECOND FROM tm6) FROM X",
            vec!["-123456", "-8"],
        ),
        // A string spelling the same instant, which is read by the same rules
        // once it parses as a datetime.
        (
            "SELECT EXTRACT(YEAR FROM '2024-01-05 06:07:08.123456'), \
             EXTRACT(HOUR FROM '2024-01-05 06:07:08.123456'), \
             EXTRACT(MICROSECOND FROM '2024-01-05 06:07:08.123456')",
            vec!["2024", "6", "123456"],
        ),
        // A bare TIME string, which only the clock units read.
        (
            "SELECT EXTRACT(HOUR FROM '06:07:08'), EXTRACT(MINUTE FROM '06:07:08'), \
             EXTRACT(SECOND FROM '06:07:08')",
            vec!["6", "7", "8"],
        ),
        // NULL in, NULL out, for a calendar unit and a clock one alike.
        (
            "SELECT EXTRACT(YEAR FROM NULL), EXTRACT(SECOND FROM NULL), \
             EXTRACT(WEEK FROM NULL)",
            vec!["NULL", "NULL", "NULL"],
        ),
        // A temporal *expression* rather than a column, which is the case the
        // value's tag exists for: MySQL types DATE_ADD a string and still
        // reads it as a date, so the clock fields are 0 and not 20 minutes
        // past the hour the leading `2024` would spell.
        (
            "SELECT EXTRACT(DAY FROM DATE_ADD('2024-01-05', INTERVAL 1 DAY)), \
             EXTRACT(SECOND FROM DATE_ADD('2024-01-05', INTERVAL 1 DAY)), \
             EXTRACT(HOUR FROM CAST('2024-01-05 06:07:08' AS DATETIME))",
            vec!["6", "0", "6"],
        ),
    ] {
        assert_eq!(rows(&engine, &mut session, sql), vec![expected], "{sql}");
    }
}

#[test]
fn extract_carries_a_negative_time_s_sign_into_every_clock_field() {
    // The sharpest measured disagreement in the whole construct: over a TIME
    // of `-06:07:08`, `EXTRACT(HOUR FROM t)` is -6 on 8.3 where `HOUR(t)` over
    // the same value is 6. So the sign cannot live in the shared stamp, and
    // the two spellings of "the hour" really do answer differently.
    let (engine, mut session) = engine();
    extractable(&engine, &mut session);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT EXTRACT(HOUR FROM tmneg), EXTRACT(MINUTE FROM tmneg), \
             EXTRACT(SECOND FROM tmneg), EXTRACT(MICROSECOND FROM tm6), \
             HOUR(tmneg), MINUTE(tmneg) FROM X"
        ),
        vec![vec!["-6", "-7", "-8", "-123456", "6", "7"]]
    );
    // And the calendar fields never take it: the same value's year is this
    // year, not minus it.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT EXTRACT(YEAR FROM tmneg) > 0, EXTRACT(MONTH FROM tmneg) > 0 FROM X"
        ),
        vec![vec!["1", "1"]]
    );
    // A string TIME carries it too, which is the path a column does not take.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT EXTRACT(HOUR FROM '-06:07:08'), EXTRACT(SECOND FROM '-06:07:08')"
        ),
        vec![vec!["-6", "-8"]]
    );
}

#[test]
fn a_time_has_no_calendar_so_extract_puts_it_on_the_current_date() {
    // A calendar unit converts the argument to a DATETIME, and a TIME has no
    // date of its own, so MySQL adds it to the current date: over a TIME of
    // `-06:07:08` the day is *yesterday* until the clock passes six in the
    // morning. Asserted against the engine's own CURDATE rather than a fixed
    // day, since a literal would fail once and then pass for a year.
    let (engine, mut session) = engine();
    extractable(&engine, &mut session);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT EXTRACT(YEAR FROM tm) = YEAR(CURDATE()), \
             EXTRACT(MONTH FROM tm) = MONTH(CURDATE()), \
             EXTRACT(DAY FROM tm) = DAY(CURDATE()), \
             EXTRACT(WEEK FROM tm) = EXTRACT(WEEK FROM CURDATE()) FROM X"
        ),
        vec![vec!["1", "1", "1", "1"]]
    );
    // The negative one runs *backwards* from midnight, so it lands on
    // yesterday. Asserted against midnight shifted by the same 6h07m08s —
    // 22028 seconds — rather than against a day, which makes it a claim about
    // the sign rather than about when the test happens to run.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT EXTRACT(DAY FROM tmneg) = DAY(DATE_SUB(CURDATE(), INTERVAL 22028 SECOND)), \
             EXTRACT(MONTH FROM tmneg) = MONTH(DATE_SUB(CURDATE(), INTERVAL 22028 SECOND)), \
             EXTRACT(DAY FROM tm) = DAY(DATE_ADD(CURDATE(), INTERVAL 22028 SECOND)) FROM X"
        ),
        vec![vec!["1", "1", "1"]]
    );
}

#[test]
fn extract_week_follows_default_week_format() {
    // MySQL's WEEK has eight modes and EXTRACT takes no argument for one, so
    // it reads `default_week_format`. Every row here was read off 8.3 under
    // that variable set to each value in turn; the dates are the ones that
    // separate the modes, where a January day belongs to the year before or a
    // December day to the year after.
    let (engine, mut session) = engine();
    // The last two are centuries: 1900 was not a leap year and 2000 was, and
    // the modes that let January belong to the year before count back over
    // exactly that many days.
    let week = "SELECT EXTRACT(WEEK FROM '2024-01-05'), EXTRACT(WEEK FROM '2023-01-01'), \
                EXTRACT(WEEK FROM '2026-12-31'), EXTRACT(WEEK FROM '2020-12-31'), \
                EXTRACT(WEEK FROM '2015-01-01'), EXTRACT(WEEK FROM '2000-02-29'), \
                EXTRACT(WEEK FROM '1901-01-01'), EXTRACT(WEEK FROM '2101-01-01')";
    // 0 is the default a stock server reports, so the first line also says
    // what an untouched session answers.
    assert_eq!(
        rows(&engine, &mut session, "SELECT @@default_week_format"),
        vec![vec!["0"]]
    );
    for (format, expected) in [
        (0, vec!["0", "1", "52", "52", "0", "9", "0", "0"]),
        (1, vec!["1", "0", "53", "53", "1", "9", "1", "0"]),
        (2, vec!["53", "1", "52", "52", "52", "9", "52", "52"]),
        (3, vec!["1", "52", "53", "53", "1", "9", "1", "52"]),
        (4, vec!["1", "1", "52", "53", "0", "9", "1", "0"]),
        (5, vec!["1", "0", "52", "52", "0", "9", "0", "0"]),
        (6, vec!["1", "1", "52", "53", "53", "9", "1", "52"]),
        (7, vec!["1", "52", "52", "52", "52", "9", "53", "52"]),
    ] {
        run(
            &engine,
            &mut session,
            &format!("SET @@default_week_format = {format}"),
        );
        assert_eq!(
            rows(&engine, &mut session, week),
            vec![expected],
            "{format}"
        );
    }
}

#[test]
fn extract_refuses_the_readings_it_cannot_make_exactly() {
    // MySQL reads several shapes this engine's parser does not, and reads them
    // as something other than nothing: `'06:07:08'` is the *date* 2006-07-08
    // to a calendar unit, `'2024-01-05'` is the *time* 00:20:24 to a clock
    // one, `'2024-01-05T06:07:08'` is 00:20:24 as well because its TIME reader
    // does not take the `T`, and a YEAR of 2024 is 00:20:24 too. Answering
    // NULL for any of them would be a wrong answer rather than a gap, so they
    // are refused — and every one of them is a shape a *typed* temporal
    // beside it answers exactly.
    let (engine, mut session) = engine();
    extractable(&engine, &mut session);
    for sql in [
        "SELECT EXTRACT(YEAR FROM '06:07:08')",
        "SELECT EXTRACT(WEEK FROM '06:07:08')",
        "SELECT EXTRACT(SECOND FROM '2024-01-05')",
        "SELECT EXTRACT(SECOND FROM '2024-01-05T06:07:08')",
        // The same shape with whitespace at either end, which MySQL still
        // answers 24 for. Both halves of the guard have to read the same,
        // trimmed text: measuring the length after trimming while looking for
        // the space before it let the padding answer the space test on its
        // own, and `temporal::parse` then took the `T` and answered 8.
        "SELECT EXTRACT(SECOND FROM '2024-01-05T06:07:08 ')",
        "SELECT EXTRACT(SECOND FROM ' 2024-01-05T06:07:08')",
        // Eleven characters, so MySQL reads it as a TIME and answers a
        // SECOND of 4 where the twelve-character spelling of the same shape
        // answers 8. The length really is the switch, and it is measured.
        "SELECT EXTRACT(SECOND FROM '4-1-5 6:7:8')",
        "SELECT EXTRACT(HOUR FROM '4-1-5 6:7:8')",
        // The same eleven characters behind a space. MySQL still answers 4, so
        // it does not count leading whitespace toward the twelve — measuring
        // the length before trimming would push this over the line and answer
        // 8. (A *trailing* space does count there: `'4-1-5 6:7:8 '` is 8. This
        // engine refuses that one, which is a refusal rather than a wrong
        // answer, and it is why the length is read after trimming.)
        "SELECT EXTRACT(SECOND FROM ' 4-1-5 6:7:8')",
        "SELECT EXTRACT(SECOND FROM '  4-1-5 6:7:8')",
        "SELECT EXTRACT(YEAR FROM 'hello')",
        "SELECT EXTRACT(YEAR FROM 20240105)",
        "SELECT EXTRACT(SECOND FROM 20240105060708)",
        "SELECT EXTRACT(YEAR FROM y) FROM X",
        "SELECT EXTRACT(SECOND FROM y) FROM X",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1235, "expected a refusal for: {sql}");
    }
    // The same readings the tag makes exact, so the refusals above are about
    // what cannot be read and not about the units.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT EXTRACT(SECOND FROM d), EXTRACT(YEAR FROM tm) > 0, \
             EXTRACT(SECOND FROM s), EXTRACT(SECOND FROM '24-1-5 6:7:8') FROM X"
        ),
        vec![vec!["0", "1", "8", "8"]]
    );
    // Whitespace at either end changes nothing about which reader MySQL uses,
    // so the space-separated datetime and the bare TIME both still answer 8.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT EXTRACT(SECOND FROM ' 2024-01-05 06:07:08'), \
             EXTRACT(SECOND FROM '2024-01-05 06:07:08 '), \
             EXTRACT(SECOND FROM '  06:07:08  '), \
             EXTRACT(YEAR FROM ' 2024-01-05 ')"
        ),
        vec![vec!["8", "8", "8", "2024"]]
    );
    // A typed temporal holding something that is not one, which MySQL has no
    // way to reach — it refuses the write — and this engine does, because
    // nothing validates on the way in. NULL rather than a refusal, which is
    // what every neighbouring reader of the same column already answers.
    run(&engine, &mut session, "CREATE TABLE J (d DATE NULL)");
    run(&engine, &mut session, "INSERT INTO J VALUES ('hello')");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT EXTRACT(SECOND FROM d), EXTRACT(YEAR FROM d) FROM J"
        ),
        vec![vec!["NULL", "NULL"]]
    );
}

#[test]
fn extract_refuses_a_unit_mysql_does_not_take() {
    // sqlparser recognises a pile of units from other dialects. MySQL calls
    // each of them a syntax error, so answering one would invent a feature the
    // engine exists to be compatible with the absence of.
    let (engine, mut session) = engine();
    for sql in [
        "SELECT EXTRACT(DOW FROM NOW())",
        "SELECT EXTRACT(EPOCH FROM NOW())",
        "SELECT EXTRACT(ISOYEAR FROM NOW())",
        "SELECT EXTRACT(YEARS FROM NOW())",
        "SELECT EXTRACT(DAYOFWEEK FROM NOW())",
        "SELECT EXTRACT(MILLISECOND FROM NOW())",
        "SELECT EXTRACT(TIMEZONE_HOUR FROM NOW())",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1235, "expected a refusal for: {sql}");
    }
}

#[test]
fn an_aggregate_and_a_grouped_column_are_seen_through_extract() {
    // `EXTRACT` is a node of its own rather than a call, so the two walks that
    // have to look inside an expression did not: an aggregate under one was
    // never folded out and reached the function table as unsupported, and a
    // column under one was invisible to ONLY_FULL_GROUP_BY, which then
    // answered with an arbitrary row. Both are the failure the `CEIL(SUM(x))`
    // comment beside those walks already describes, one variant later.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE A (id INT NOT NULL, d DATE NULL, s VARCHAR(8) NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO A VALUES (1, '2024-01-05', 'x'), (2, '2025-06-30', 'x')",
    );
    for (sql, expected) in [
        ("SELECT EXTRACT(YEAR FROM MAX(d)) FROM A", "2025"),
        ("SELECT EXTRACT(MONTH FROM MIN(d)) FROM A", "1"),
        ("SELECT SUM(EXTRACT(MONTH FROM d)) FROM A", "7"),
        (
            "SELECT COUNT(*) FROM A HAVING EXTRACT(YEAR FROM MAX(d)) = 2025",
            "2",
        ),
        // The same aggregate spelled as a call, which already worked and is
        // here so the pair says the two spellings now agree.
        ("SELECT YEAR(MAX(d)) FROM A", "2025"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM A ORDER BY EXTRACT(MONTH FROM d) DESC, id"
        ),
        vec![vec!["2"], vec!["1"]]
    );
    // A column reachable only through an `EXTRACT` is still a nonaggregated
    // column, and grouping by something it does not depend on is 1055 — which
    // this engine answered as a row until the walk saw inside.
    let err = engine
        .execute(
            &mut session,
            "SELECT EXTRACT(YEAR FROM d) FROM A GROUP BY s",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1055);
}

#[test]
fn default_week_format_is_bounded_the_way_mysql_bounds_it() {
    // The one system variable this engine reads back into an answer, so the
    // value it stores has to be the value MySQL would store. Measured on 8.3:
    // out of range is *clamped* with warning 1292 rather than refused, so 9 is
    // 7 and -1 is 0 — where masking it to three bits, which is what
    // `week_mode` does to the value once stored, would make 9 mean 1 and
    // answer week 1 for a date MySQL puts in week 52.
    let (engine, mut session) = engine();
    for (set, stored, week) in [
        ("9", "7", "52"),
        // Above `i64::MAX`, which arrives as a `Value::Unsigned`. Read through
        // `as_int` it wraps to -1 and clamps *down* to 0, answering week 1 for
        // a date MySQL puts in week 52.
        ("18446744073709551615", "7", "52"),
        ("8", "7", "52"),
        ("7", "7", "52"),
        ("-1", "0", "1"),
        ("0", "0", "1"),
        ("1", "1", "0"),
        // An expression and a keyword, both of which MySQL reads as the
        // numbers they are.
        ("1 + 1", "2", "1"),
        ("TRUE", "1", "0"),
    ] {
        run(
            &engine,
            &mut session,
            &format!("SET SESSION default_week_format = {set}"),
        );
        assert_eq!(
            rows(
                &engine,
                &mut session,
                "SELECT @@default_week_format, EXTRACT(WEEK FROM '2023-01-01')"
            ),
            vec![vec![stored, week]],
            "{set}"
        );
    }
    // And a value that is not an integer is refused rather than taken, which
    // includes a string spelling one: `SET default_week_format = '3'` is 1232
    // on 8.3, not three.
    // `b'11'` is deliberately absent: MySQL refuses it with 1232 too, but this
    // engine has no bit literal at all and answers 1235 for the literal rather
    // than for the variable — a different gap wearing the same shape.
    for set in ["'abc'", "'3'", "3.7", "NULL"] {
        let err = engine
            .execute(
                &mut session,
                &format!("SET SESSION default_week_format = {set}"),
                &[],
            )
            .unwrap_err();
        assert_eq!(err.code, 1232, "expected 1232 for: {set}");
    }
    // A variable the engine answers is a variable SHOW VARIABLES lists, and it
    // has to be there on a session that has never set one: listed only after a
    // write is worse than absent, because the list would then describe two
    // different servers either side of a `SET` nothing else can see.
    //
    // Asserted by looking for the row rather than by filtering, because this
    // engine's `SHOW VARIABLES` ignores its `LIKE` and hands back the whole
    // list — a gap of its own, and not one this claim depends on.
    let (fresh, mut clean) = fresh_engine();
    let listed = |engine: &Engine, session: &mut Session| -> Vec<String> {
        rows(engine, session, "SHOW VARIABLES")
            .into_iter()
            .filter(|row| row[0] == "default_week_format")
            .map(|row| row[1].clone())
            .collect()
    };
    assert_eq!(listed(&fresh, &mut clean), vec!["0".to_owned()]);
    // And it follows the session's own value once one is set.
    assert_eq!(listed(&engine, &mut session), vec!["1".to_owned()]);
}

/// The source table the `CREATE TABLE ... AS SELECT` tests below build from,
/// declared so that every attribute MySQL drops on the way through has
/// something to drop: an AUTO_INCREMENT primary key, a unique key, a NOT NULL
/// column with a default, a nullable one with a default, and two with none.
fn ctas_source(engine: &Engine, session: &mut Session) {
    run(
        engine,
        session,
        "CREATE TABLE Src (
            id INT NOT NULL AUTO_INCREMENT,
            name VARCHAR(40) NOT NULL DEFAULT 'x',
            qty INT UNSIGNED NULL DEFAULT 7,
            note TEXT NULL,
            amt DECIMAL(10,2) NULL,
            PRIMARY KEY (id),
            UNIQUE KEY uk_name (name)
        )",
    );
    run(
        engine,
        session,
        "INSERT INTO Src (name, qty, note, amt) VALUES ('a', 1, 'n1', 1.50), ('b', 2, 'n2', 2.50)",
    );
}

/// What `information_schema` reports for every column of a table, in order,
/// which is where a derived declaration is observable at all.
fn declared(engine: &Engine, session: &mut Session, table: &str) -> Vec<Vec<String>> {
    rows(
        engine,
        session,
        &format!(
            "SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE, COLUMN_KEY, EXTRA \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = '{table}' ORDER BY ORDINAL_POSITION"
        ),
    )
}

/// `CREATE TABLE ... AS SELECT` declares the table from its query's result
/// metadata and fills it with the query's rows.
///
/// Every expectation here was measured against mysql:8.3 with `SHOW CREATE
/// TABLE` and `information_schema.columns`; the differential corpus asks the
/// same questions of both servers, and this pins the answers where a change to
/// them has to be deliberate.
///
/// Mismatches are collected rather than asserted one at a time, so a mutation
/// reports every claim it broke instead of the first.
#[test]
fn create_table_as_select_declares_its_columns_and_inserts_its_rows() {
    let (engine, mut session) = engine();
    ctas_source(&engine, &mut session);

    let mut wrong: Vec<String> = Vec::new();
    fn check(wrong: &mut Vec<String>, claim: &str, got: Vec<Vec<String>>, want: Vec<Vec<&str>>) {
        let want: Vec<Vec<String>> = want
            .into_iter()
            .map(|row| row.into_iter().map(str::to_owned).collect())
            .collect();
        if got != want {
            wrong.push(format!("{claim}\n  got  {got:?}\n  want {want:?}"));
        }
    }

    // A copied column keeps its type, its nullability and its DEFAULT, and
    // loses everything else: the AUTO_INCREMENT and both keys are gone, which
    // is the whole difference between this and LIKE. A NOT NULL column with no
    // default to copy gets the type's implicit one, which is where `id`'s
    // DEFAULT 0 comes from -- a plain CREATE TABLE would leave it with none.
    let inserted = affected(
        &engine,
        &mut session,
        "CREATE TABLE Star AS SELECT * FROM Src",
    );
    if inserted != 2 {
        wrong.push(format!("SELECT * inserted {inserted} rows, want 2"));
    }
    check(
        &mut wrong,
        "SELECT *",
        declared(&engine, &mut session, "Star"),
        vec![
            vec!["id", "0", "NO", "int", "", ""],
            vec!["name", "x", "NO", "varchar(40)", "", ""],
            vec!["qty", "7", "YES", "int unsigned", "", ""],
            vec!["note", "NULL", "YES", "text", "", ""],
            vec!["amt", "NULL", "YES", "decimal(10,2)", "", ""],
        ],
    );
    check(
        &mut wrong,
        "SELECT * rows",
        rows(&engine, &mut session, "SELECT * FROM Star ORDER BY id"),
        vec![
            vec!["1", "a", "1", "n1", "1.50"],
            vec!["2", "b", "2", "n2", "2.50"],
        ],
    );

    // An expression is typed by what it computes, and takes the implicit
    // default where it is NOT NULL. `id + 1` is NOT NULL because `id` is.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Expr AS SELECT id + 1 AS a, name FROM Src",
    );
    check(
        &mut wrong,
        "an expression beside a copied column",
        declared(&engine, &mut session, "Expr"),
        vec![
            vec!["a", "0", "NO", "bigint", "", ""],
            vec!["name", "x", "NO", "varchar(40)", "", ""],
        ],
    );

    // An all-NULL column is `binary(0)`, which is what #0073 measured this
    // very statement to settle.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Nul AS SELECT NULL AS a",
    );
    check(
        &mut wrong,
        "SELECT NULL",
        declared(&engine, &mut session, "Nul"),
        vec![vec!["a", "NULL", "YES", "binary(0)", "", ""]],
    );

    // An aggregate keeps the value it computed, at the scale it computed it.
    // The engine's derived type for it is only `decimal`, because a result
    // column carries no scale, and coercing into that would have written 2.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Agg AS SELECT COUNT(*) AS c, SUM(qty) AS s, AVG(amt) AS av FROM Src",
    );
    check(
        &mut wrong,
        "an aggregate's rows",
        rows(&engine, &mut session, "SELECT * FROM Agg"),
        vec![vec!["2", "3", "2.000000"]],
    );

    // An empty result still declares the table.
    let none = affected(
        &engine,
        &mut session,
        "CREATE TABLE Empty AS SELECT id FROM Src WHERE 0",
    );
    if none != 0 {
        wrong.push(format!("an empty result inserted {none} rows, want 0"));
    }
    check(
        &mut wrong,
        "an empty result",
        declared(&engine, &mut session, "Empty"),
        vec![vec!["id", "0", "NO", "int", "", ""]],
    );

    // Explicit definitions are merged with the query's columns rather than
    // replacing them: one the query does not name keeps its own place in
    // front, and one it does names *its* column, in the query's position.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Ex1 (z BIGINT, y VARCHAR(5)) AS SELECT id, name FROM Src",
    );
    check(
        &mut wrong,
        "explicit columns the query does not name",
        declared(&engine, &mut session, "Ex1"),
        vec![
            vec!["z", "NULL", "YES", "bigint", "", ""],
            vec!["y", "NULL", "YES", "varchar(5)", "", ""],
            vec!["id", "0", "NO", "int", "", ""],
            vec!["name", "x", "NO", "varchar(40)", "", ""],
        ],
    );
    check(
        &mut wrong,
        "an unfilled explicit column takes its default",
        rows(&engine, &mut session, "SELECT * FROM Ex1 ORDER BY id"),
        vec![
            vec!["NULL", "NULL", "1", "a"],
            vec!["NULL", "NULL", "2", "b"],
        ],
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE Ex2 (name VARCHAR(5)) AS SELECT id, name FROM Src",
    );
    check(
        &mut wrong,
        "an explicit column the query names",
        declared(&engine, &mut session, "Ex2"),
        vec![
            vec!["id", "0", "NO", "int", "", ""],
            vec!["name", "NULL", "YES", "varchar(5)", "", ""],
        ],
    );

    // An AUTO_INCREMENT column the query does not fill is assigned as if the
    // rows had been inserted -- measured on 8.3, where `seq` counts 1 and 2.
    // It had been taking the type's implicit default, so every row of a
    // PRIMARY KEY column held 0.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Auto (seq INT NOT NULL AUTO_INCREMENT PRIMARY KEY) AS SELECT id FROM Src",
    );
    check(
        &mut wrong,
        "an unfilled AUTO_INCREMENT column",
        rows(&engine, &mut session, "SELECT * FROM Auto ORDER BY seq"),
        vec![vec!["1", "1"], vec!["2", "2"]],
    );
    // And the ids it generated are reported the way MySQL reports them: the
    // *first*, not the last, which is 1 for these two rows on 8.3.
    check(
        &mut wrong,
        "LAST_INSERT_ID() after a CTAS",
        rows(&engine, &mut session, "SELECT LAST_INSERT_ID()"),
        vec![vec!["1"]],
    );

    // An alias renames the result column and leaves the underlying one alone,
    // so the declaration follows the source column. Following the *name*
    // found `Src.name` -- a different column -- and copied its VARCHAR default
    // into an INT one, which then stored the string 'x'.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Alias AS SELECT id AS name FROM Src",
    );
    check(
        &mut wrong,
        "an alias takes the source column's attributes",
        declared(&engine, &mut session, "Alias"),
        vec![vec!["name", "0", "NO", "int", "", ""]],
    );
    // Two columns of one type and different defaults, so the source is
    // distinguishable from the name even where the types agree.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Pair (a INT DEFAULT 1, b INT DEFAULT 2)",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE Renamed AS SELECT a AS b FROM Pair",
    );
    check(
        &mut wrong,
        "an alias onto another column's name, same type",
        declared(&engine, &mut session, "Renamed"),
        vec![vec!["b", "1", "YES", "int", "", ""]],
    );

    // A table *alias*, including one that shadows the name of a real table.
    // The default follows the table the alias stands for, which is what 8.3
    // does: all three of these declare `name varchar(40) NOT NULL DEFAULT 'x'`
    // there, out of `Src`.
    //
    // Looking the table up by the alias was wrong twice over. It missed the
    // plain case -- there is no table `s` for `FROM Src s` to read a default
    // out of -- and it answered the shadowed one out of a table the query
    // never touched. A type check narrowed the second to the differently-typed
    // spelling and left `Other2` below, whose `name` is the same VARCHAR(40),
    // copying `'z'` where the source column says `'x'`.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Other (name INT DEFAULT 5)",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE Other2 (name VARCHAR(40) DEFAULT 'z')",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE Shadow AS SELECT Other.name FROM Src Other",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE Shadow2 AS SELECT Other2.name FROM Src Other2",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE Aliased AS SELECT s.name FROM Src s",
    );
    for table in ["Shadow", "Shadow2", "Aliased"] {
        check(
            &mut wrong,
            &format!("a default copied through the alias in {table}"),
            declared(&engine, &mut session, table),
            vec![vec!["name", "x", "NO", "varchar(40)", "", ""]],
        );
    }

    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The statements MySQL refuses, refused for the same reasons and with the
/// same codes. Measured on 8.3.
#[test]
fn create_table_as_select_refuses_what_mysql_refuses() {
    let (engine, mut session) = engine();
    ctas_source(&engine, &mut session);

    let mut wrong: Vec<String> = Vec::new();
    let mut refuses = |sql: &str, code: u16| match engine.execute(&mut session, sql, &[]) {
        Err(e) if e.code == code => {}
        Err(e) => wrong.push(format!("{sql}\n  -> {e}, want {code}")),
        Ok(_) => wrong.push(format!("{sql}\n  -> accepted, want {code}")),
    };

    // Two columns of one name is not a table, however it arose -- including
    // the plain spelling, which the check missed for as long as it lived on
    // the `AS SELECT` path: `CREATE TABLE t (a INT, a INT)` made exactly the
    // table the refusal exists to prevent.
    refuses("CREATE TABLE D1 AS SELECT id, id FROM Src", 1060);
    refuses(
        "CREATE TABLE D2 AS SELECT * FROM Src a JOIN Src b ON a.id = b.id",
        1060,
    );
    refuses("CREATE TABLE D4 (a INT, a INT)", 1060);
    // Case-insensitively, as MySQL compares column names.
    refuses("CREATE TABLE D5 (a INT, A INT)", 1060);
    // Before the key is looked at, which is why the check runs ahead of
    // `apply_constraint` rather than after it: 8.3 answers this 1060, not the
    // 1072 the key alone would raise there. This engine answers 1060 for the
    // right reason and not by accident -- it does not raise 1072 for a key
    // naming an unknown column at all, which is a separate gap and is why
    // there is no case for that spelling here.
    refuses("CREATE TABLE D6 (a INT, a INT, PRIMARY KEY (nope))", 1060);
    // The query is resolved before the name is checked, so a bad query is
    // reported as a bad query even under IF NOT EXISTS on a name in use.
    refuses("CREATE TABLE D3 AS SELECT * FROM Nope", 1146);
    refuses("CREATE TABLE IF NOT EXISTS Src AS SELECT * FROM Nope", 1146);
    refuses("CREATE TABLE Src AS SELECT 1 AS a", 1050);
    // `CREATE TABLE t ()` is a syntax error on 8.3; `sqlparser` parses it, so
    // the engine is the only thing that can refuse it.
    refuses("CREATE TABLE Z ()", 1064);

    // The rows go in on the path INSERT writes by, so every row-level rule the
    // new table declares applies to them. Each of these was accepted before,
    // and left a table holding rows its own constraints forbid -- the CHECK
    // one most plainly: it stored 1 and 2 and then refused an insert of 5.
    refuses(
        "CREATE TABLE Dup (k INT PRIMARY KEY) AS SELECT 1 AS k UNION ALL SELECT 1",
        1062,
    );
    refuses(
        "CREATE TABLE Bad (a INT NOT NULL) AS SELECT NULL AS a",
        1048,
    );
    refuses(
        "CREATE TABLE Chk (id INT, CHECK (id > 100)) AS SELECT id FROM Src",
        3819,
    );
    // And a refusal leaves no table behind, which is what 8.3 does: nothing
    // reaches the catalog until every row has been admitted.
    for name in ["Dup", "Bad", "Chk"] {
        let left = declared(&engine, &mut session, name);
        if !left.is_empty() {
            wrong.push(format!("a refused CTAS left {name} behind: {left:?}"));
        }
    }

    // And IF NOT EXISTS on a name in use is a success that changes nothing.
    match engine.execute(
        &mut session,
        "CREATE TABLE IF NOT EXISTS Src AS SELECT 1 AS a",
        &[],
    ) {
        Ok(_) => {
            let columns = declared(&engine, &mut session, "Src");
            if columns.len() != 5 {
                wrong.push(format!("IF NOT EXISTS replaced Src: {columns:?}"));
            }
        }
        Err(e) => wrong.push(format!("IF NOT EXISTS on a name in use -> {e}")),
    }

    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// `CREATE TABLE ... LIKE` is the opposite of `AS SELECT` in what it keeps:
/// the whole definition, and none of the rows.
#[test]
fn create_table_like_copies_the_definition_and_none_of_the_rows() {
    let (engine, mut session) = engine();
    ctas_source(&engine, &mut session);
    run(&engine, &mut session, "CREATE TABLE Copy LIKE Src");

    let mut wrong: Vec<String> = Vec::new();
    // The AUTO_INCREMENT and both keys survive, which `AS SELECT` drops.
    let want: Vec<Vec<String>> = vec![
        vec!["id", "NULL", "NO", "int", "PRI", "auto_increment"],
        vec!["name", "x", "NO", "varchar(40)", "UNI", ""],
        vec!["qty", "7", "YES", "int unsigned", "", ""],
        vec!["note", "NULL", "YES", "text", "", ""],
        vec!["amt", "NULL", "YES", "decimal(10,2)", "", ""],
    ]
    .into_iter()
    .map(|row| row.into_iter().map(str::to_owned).collect())
    .collect();
    let got = declared(&engine, &mut session, "Copy");
    if got != want {
        wrong.push(format!("LIKE\n  got  {got:?}\n  want {want:?}"));
    }

    // No rows, and the counter starts over rather than resuming the source's.
    let count = rows(&engine, &mut session, "SELECT COUNT(*) FROM Copy");
    if count != vec![vec!["0".to_owned()]] {
        wrong.push(format!("LIKE copied rows: {count:?}"));
    }
    run(
        &engine,
        &mut session,
        "INSERT INTO Copy (name) VALUES ('z')",
    );
    let first = rows(&engine, &mut session, "SELECT id FROM Copy");
    if first != vec![vec!["1".to_owned()]] {
        wrong.push(format!("the copy's first id was {first:?}, want 1"));
    }

    // `CREATE TABLE Copy2 (LIKE Src)` is the same statement to MySQL and is
    // deliberately absent: `sqlparser`'s MySQL dialect reads it as a column
    // named LIKE of type Src, so the engine answers 1235 for a column type
    // rather than copying anything. A clear error rather than a wrong answer,
    // and a parser gap of its own -- the arm that would handle it is written
    // and unreachable until the parser produces it.

    // A view is in the same namespace and is not a base table.
    run(
        &engine,
        &mut session,
        "CREATE VIEW VSrc AS SELECT id FROM Src",
    );
    for (sql, code) in [
        ("CREATE TABLE C3 LIKE VSrc", 1347u16),
        ("CREATE TABLE C4 LIKE Nope", 1146),
        ("CREATE TABLE Copy LIKE Src", 1050),
        // A table cannot be copied from itself, and 8.3 says so before it
        // looks anything up: the same statement over a name that does not
        // exist at all is 1066 too, not 1146.
        ("CREATE TABLE Src LIKE Src", 1066),
        ("CREATE TABLE Nope2 LIKE Nope2", 1066),
        ("CREATE TABLE IF NOT EXISTS Src LIKE Src", 1066),
    ] {
        match engine.execute(&mut session, sql, &[]) {
            Err(e) if e.code == code => {}
            Err(e) => wrong.push(format!("{sql}\n  -> {e}, want {code}")),
            Ok(_) => wrong.push(format!("{sql}\n  -> accepted, want {code}")),
        }
    }

    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A table with no columns cannot be answered for -- a result set has no
/// encoding for zero columns -- so no statement may leave one behind.
///
/// The engine's own half of #0125: the wire refuses to send such a result at
/// all, and these are the routes that used to produce one. Both were measured
/// on 8.3, which refuses them too and with these codes.
#[test]
fn no_statement_leaves_a_table_with_no_columns() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE One (a INT, b INT)");

    let mut wrong: Vec<String> = Vec::new();
    for (sql, code) in [
        // `sqlparser` accepts the empty column list that MySQL's grammar does
        // not, so this is the engine's to refuse.
        ("CREATE TABLE Zero ()", 1064u16),
    ] {
        match engine.execute(&mut session, sql, &[]) {
            Err(e) if e.code == code => {}
            Err(e) => wrong.push(format!("{sql}\n  -> {e}, want {code}")),
            Ok(_) => wrong.push(format!("{sql}\n  -> accepted, want {code}")),
        }
    }

    // Dropping columns is fine until the last one, which is the point: the
    // guard is about the table being emptied, not about DROP COLUMN.
    run(&engine, &mut session, "ALTER TABLE One DROP COLUMN b");
    match engine.execute(&mut session, "ALTER TABLE One DROP COLUMN a", &[]) {
        Err(e) if e.code == 1090 => {}
        Err(e) => wrong.push(format!("dropping the last column -> {e}, want 1090")),
        Ok(_) => wrong.push("dropping the last column was accepted".to_owned()),
    }
    // And the column it refused to drop is still there.
    let left = declared(&engine, &mut session, "One");
    if left.len() != 1 {
        wrong.push(format!("the refused drop still took effect: {left:?}"));
    }

    // One ALTER is one statement however many operations it lists. This
    // answered 1090 -- correctly -- and dropped `a` and its data anyway,
    // because the loop mutated the stored table in place and the guard fired
    // on the second operation.
    run(&engine, &mut session, "CREATE TABLE Two (a INT, b INT)");
    run(&engine, &mut session, "INSERT INTO Two VALUES (1, 2)");
    match engine.execute(
        &mut session,
        "ALTER TABLE Two DROP COLUMN a, DROP COLUMN b",
        &[],
    ) {
        Err(e) if e.code == 1090 => {}
        Err(e) => wrong.push(format!("dropping both columns -> {e}, want 1090")),
        Ok(_) => wrong.push("dropping both columns was accepted".to_owned()),
    }
    let names: Vec<String> = declared(&engine, &mut session, "Two")
        .into_iter()
        .map(|row| row[0].clone())
        .collect();
    if names != vec!["a".to_owned(), "b".to_owned()] {
        wrong.push(format!("a refused ALTER was partly applied: {names:?}"));
    }
    let kept = rows(&engine, &mut session, "SELECT a, b FROM Two");
    if kept != vec![vec!["1".to_owned(), "2".to_owned()]] {
        wrong.push(format!("a refused ALTER lost the rows: {kept:?}"));
    }

    // The same hazard reached by a different operation: an unknown column in
    // the second position must undo the first, which is why the whole
    // statement runs against a copy rather than the guard being special.
    //
    // The code is the engine's own and is asserted as such: 8.3 answers 1091
    // `Can't DROP 'nosuch'` where this answers 1054 `Unknown column`. That is
    // a separate, pre-existing gap in DROP COLUMN and not what this is about
    // -- which is that both columns are still there afterwards, and they are
    // on both servers.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Three (a INT, b INT, c INT)",
    );
    match engine.execute(
        &mut session,
        "ALTER TABLE Three DROP COLUMN a, DROP COLUMN nosuch",
        &[],
    ) {
        Err(e) if e.code == 1054 => {}
        Err(e) => wrong.push(format!("dropping an unknown column -> {e}, want 1054")),
        Ok(_) => wrong.push("dropping an unknown column was accepted".to_owned()),
    }
    let names: Vec<String> = declared(&engine, &mut session, "Three")
        .into_iter()
        .map(|row| row[0].clone())
        .collect();
    if names != vec!["a".to_owned(), "b".to_owned(), "c".to_owned()] {
        wrong.push(format!("a refused ALTER was partly applied: {names:?}"));
    }

    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Every number here was measured against MySQL 8.3 rather than derived, by
/// declaring the column and reading `column_length` and the flags word back
/// off the wire. See `crates/differential/README.md`, which is where the
/// harness now compares them.
#[test]
fn a_column_is_described_with_the_width_it_was_declared_with() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (\
           c1 CHAR(1), c10 CHAR(10), v10 VARCHAR(10), v1000 VARCHAR(1000), \
           b16 BINARY(16), vb9 VARBINARY(9), \
           tt TINYTEXT, t TEXT, mt MEDIUMTEXT, lt LONGTEXT, \
           tb TINYBLOB, bb BLOB, mb MEDIUMBLOB, lb LONGBLOB, \
           e ENUM('a','bb','ccc'), s SET('a','bb','ccc'), j JSON, \
           dt DATE, tm TIME, tm3 TIME(3), dtm DATETIME, dtm6 DATETIME(6), \
           ts TIMESTAMP NULL, ts2 TIMESTAMP(2) NULL, y YEAR, \
           ti TINYINT, tiu TINYINT UNSIGNED, ti1 TINYINT(1), bo BOOLEAN, \
           si SMALLINT, siu SMALLINT UNSIGNED, mi MEDIUMINT, miu MEDIUMINT UNSIGNED, \
           i INT, iu INT UNSIGNED, i4 INT(4), bi BIGINT, biu BIGINT UNSIGNED, \
           f FLOAT, fd FLOAT(10,4), d DOUBLE, dd DOUBLE(12,5), \
           de DECIMAL(10,2), de0 DECIMAL(10,0), de65 DECIMAL(65,30), \
           deu DECIMAL(8,3) UNSIGNED)",
    );
    let described = engine
        .describe(&session, "SELECT * FROM D", 0)
        .unwrap()
        .unwrap();
    let widths: Vec<(String, u32)> = described
        .iter()
        .map(|c| (c.name.clone(), c.to_wire().column_length))
        .collect();
    let want: Vec<(&str, u32)> = vec![
        // A character is four bytes in utf8mb4, and the width is in bytes.
        ("c1", 4),
        ("c10", 40),
        ("v10", 40),
        ("v1000", 4000),
        // Bytes are bytes: no multiplier on a binary string.
        ("b16", 16),
        ("vb9", 9),
        ("tt", 1020),
        ("t", 262140),
        ("mt", 67108860),
        // Four times 4294967295 does not fit in the four bytes the field has,
        // so MySQL sends what does.
        ("lt", 4294967295),
        ("tb", 255),
        ("bb", 65535),
        ("mb", 16777215),
        ("lb", 4294967295),
        // The widest member, and for a SET every member and its commas.
        ("e", 12),
        ("s", 32),
        ("j", 4294967295),
        ("dt", 10),
        ("tm", 10),
        ("tm3", 14),
        ("dtm", 19),
        ("dtm6", 26),
        ("ts", 19),
        ("ts2", 22),
        ("y", 4),
        // Display widths. The unsigned one is a digit narrower because there
        // is no sign to print — except a BIGINT, whose 20 digits are already
        // what its unsigned range needs.
        ("ti", 4),
        ("tiu", 3),
        ("ti1", 1),
        ("bo", 1),
        ("si", 6),
        ("siu", 5),
        ("mi", 9),
        ("miu", 8),
        ("i", 11),
        ("iu", 10),
        ("i4", 4),
        ("bi", 20),
        ("biu", 20),
        // A declared `(M,D)` reports M; without one, what the type can print.
        ("f", 12),
        ("fd", 10),
        ("d", 22),
        ("dd", 12),
        // Digits, plus the point where there is a scale, plus the sign where
        // the column can be negative.
        ("de", 12),
        ("de0", 11),
        ("de65", 67),
        ("deu", 9),
    ];
    let want: Vec<(String, u32)> = want.into_iter().map(|(n, w)| (n.to_owned(), w)).collect();
    assert_eq!(widths, want);
}

/// The ENUM and SET flags, which this engine set for nothing at all until
/// #0087 — an `ENUM('a','b')` reached a client as an indistinguishable
/// `CHAR`, because MySQL sends both under `MYSQL_TYPE_STRING` and the flag is
/// the only thing on the wire that separates them.
///
/// Every expectation here was measured on 8.3 rather than derived. The shapes
/// that keep the flag and the shapes that lose it are both asserted: a
/// derived table, a CTE and a view keep it, a set operation materialises it
/// away, and an expression over the column never had it.
#[test]
fn an_enum_or_set_column_says_which_list_it_belongs_to() {
    use mysql_ultralight_protocol::column::flags;

    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE M (\
           e ENUM('a','bb','ccc') NULL, en ENUM('a','b') NOT NULL, \
           s SET('a','bb','ccc') NULL, sn SET('a','b') NOT NULL, \
           c CHAR(3) NULL, v VARCHAR(3) NULL)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO M VALUES ('a', 'a', 'bb', 'a', 'x', 'y')",
    );
    run(
        &engine,
        &mut session,
        "CREATE VIEW MV AS SELECT e, s FROM M",
    );

    // Named rather than compared as a number, so a failure says which bit.
    let listed = |sql: &str| -> Vec<String> {
        engine
            .describe(&session, sql, 0)
            .unwrap_or_else(|e| panic!("{sql}\n  -> {e}"))
            .unwrap_or_else(|| panic!("{sql} described nothing"))
            .iter()
            .map(|c| {
                let wire = c.to_wire();
                match (wire.flags & flags::ENUM != 0, wire.flags & flags::SET != 0) {
                    (true, false) => "ENUM".to_owned(),
                    (false, true) => "SET".to_owned(),
                    (false, false) => String::new(),
                    // MySQL declares no column that is both, so an engine
                    // sending both bits is a bug this must not read as one.
                    (true, true) => "ENUM|SET".to_owned(),
                }
            })
            .collect()
    };
    let blank = || String::new();

    // A stored column, nullable and not, beside the two string types that
    // share its type code and must stay bare.
    assert_eq!(
        listed("SELECT e, en, s, sn, c, v FROM M"),
        vec![
            "ENUM".to_owned(),
            "ENUM".to_owned(),
            "SET".to_owned(),
            "SET".to_owned(),
            blank(),
            blank(),
        ]
    );

    // The declaration survives everything that reads the column back out of a
    // table rather than computing a new one: 8.3 reports a table and the flag
    // through every one of these.
    for sql in [
        "SELECT e, s FROM (SELECT e, s FROM M) d",
        "WITH c AS (SELECT e, s FROM M) SELECT e, s FROM c",
        "SELECT e, s FROM MV",
        "SELECT e, s FROM M GROUP BY e, s",
        "SELECT DISTINCT e, s FROM M",
        "SELECT e, s FROM M ORDER BY e",
    ] {
        assert_eq!(
            listed(sql),
            vec!["ENUM".to_owned(), "SET".to_owned()],
            "{sql}"
        );
    }

    // A set operation materialises the column into a plain string, so the flag
    // goes with the ordinal: `SELECT e FROM M UNION ALL SELECT e FROM M` is a
    // VAR_STRING with no flags on 8.3, even with the same declaration on both
    // sides.
    assert_eq!(
        listed("SELECT e, s FROM M UNION ALL SELECT e, s FROM M"),
        vec![blank(), blank()]
    );

    // An expression is an item and not a field, so there is no declaration for
    // the flag to come off: 8.3 sends every one of these as a VAR_STRING or a
    // number with the ENUM bit clear.
    assert_eq!(
        listed("SELECT CONCAT(e, ''), IF(1, e, e), e + 0, CONCAT(s, ''), s + 0 FROM M"),
        vec![blank(), blank(), blank(), blank(), blank()]
    );
    // Asked on its own: an aggregate beside a bare column is not a statement
    // ONLY_FULL_GROUP_BY allows.
    assert_eq!(
        listed("SELECT MAX(e), MAX(s) FROM M"),
        vec![blank(), blank()]
    );

    // `information_schema.columns.COLUMN_KEY` is a real ENUM on 8.3 —
    // `enum('','PRI','UNI','MUL')` — and reaches the wire with the flag, so a
    // client introspecting a schema reads it as a member list. It is the one
    // column of that table this applies to.
    assert_eq!(
        listed(
            "SELECT COLUMN_NAME, COLUMN_TYPE, COLUMN_KEY, EXTRA \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'M'"
        ),
        vec![blank(), blank(), "ENUM".to_owned(), blank()]
    );
}

/// The BINARY flag, which this engine set for nothing at all until #0126.
///
/// Measured on 8.3. The rule has two halves: a column read out of a table is
/// binary when its values are bytes and never when it is a number, and an
/// expression is binary whenever its collation is — which numbers are, so
/// `SELECT 1` carries the flag where the `INT` column it came from does not.
#[test]
fn a_column_says_whether_its_values_are_bytes() {
    use mysql_ultralight_protocol::column::flags;

    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE B (i INT, v VARCHAR(10), t TEXT, b BLOB, j JSON, \
         d DATE, dt DATETIME(3), y YEAR, de DECIMAL(6,2))",
    );
    let named = |sql: &str| -> Vec<String> {
        engine
            .describe(&session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| {
                let wire = c.to_wire();
                let mut set = Vec::new();
                for (bit, name) in [
                    (flags::NOT_NULL, "NOT_NULL"),
                    (flags::BLOB, "BLOB"),
                    (flags::UNSIGNED, "UNSIGNED"),
                    (flags::ZEROFILL, "ZEROFILL"),
                    (flags::BINARY, "BINARY"),
                ] {
                    if wire.flags & bit != 0 {
                        set.push(name);
                    }
                }
                set.join("|")
            })
            .collect()
    };
    let stored = named("SELECT i, v, t, b, j, d, dt, y, de FROM B");
    assert_eq!(
        stored,
        vec![
            // A number is not binary, whatever character set it reports.
            "".to_owned(),
            "".to_owned(),
            // A TEXT is a blob and is not binary; a BLOB is both.
            "BLOB".to_owned(),
            "BLOB|BINARY".to_owned(),
            "BLOB|BINARY".to_owned(),
            "BINARY".to_owned(),
            "BINARY".to_owned(),
            // A YEAR is reported zero-filled as well as unsigned.
            "UNSIGNED|ZEROFILL".to_owned(),
            "".to_owned(),
        ]
    );

    let mut computed =
        named("SELECT 1, 'x', i + 1, NOW(), CONCAT(v, 'x'), JSON_OBJECT('a', 1) FROM B");
    // A string function whose answer this engine types as a blob: MySQL sends
    // all three in the connection's character set, `GROUP_CONCAT(v)` with no
    // flags at all on 8.3. Described binary they arrive as bytes, which is a
    // client handing back a Buffer where the query asked for text.
    let charset = |sql: &str| -> Vec<(String, u16)> {
        engine
            .describe(&session, sql, 0)
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .unwrap_or_else(|| panic!("{sql} described nothing"))
            .iter()
            .map(|c| {
                let wire = c.to_wire();
                (format!("{:?}", wire.column_type), wire.charset)
            })
            .collect()
    };
    for sql in [
        "SELECT GROUP_CONCAT(v) FROM B",
        "SELECT JSON_UNQUOTE(j) FROM B",
        "SELECT j ->> '$.a' FROM B",
    ] {
        assert_eq!(named(sql), vec!["".to_owned()], "{sql}");
        for (code, cs) in charset(sql) {
            assert_ne!(cs, 63, "{sql} was described binary, as a {code}");
        }
    }
    // Asked on its own, because an aggregate beside a bare column is not a
    // statement ONLY_FULL_GROUP_BY allows.
    computed.insert(3, named("SELECT COUNT(*) FROM B").remove(0));
    assert_eq!(
        computed,
        vec![
            // An expression over numbers is binary where the stored column is
            // not, because the flag follows the item's collation.
            "NOT_NULL|BINARY".to_owned(),
            "NOT_NULL".to_owned(),
            "BINARY".to_owned(),
            "NOT_NULL|BINARY".to_owned(),
            "NOT_NULL|BINARY".to_owned(),
            "".to_owned(),
            // A blob-typed expression carries no BLOB flag: there is no field
            // behind it for MySQL to read one off, and the stored `j` column
            // two assertions up carries both.
            "BINARY".to_owned(),
        ]
    );
}

/// Every value below measured on MySQL 8.3 rather than reasoned about, and the
/// asymmetries are why: `HEX` rounds a decimal where `BIN` truncates one, and
/// `HEX` reads a number where `UNHEX` reads the characters it prints as.
#[test]
fn hex_reads_a_string_as_bytes_and_a_number_as_its_pattern() {
    let (engine, mut session) = engine();

    for (sql, expected) in [
        // The string branch: the argument's bytes, uppercase, two characters
        // each. A multi-byte character is all of its bytes.
        ("SELECT HEX('abc')", "616263"),
        ("SELECT HEX('€abc')", "E282AC616263"),
        ("SELECT HEX('é')", "C3A9"),
        ("SELECT HEX('')", ""),
        ("SELECT HEX(NULL)", "NULL"),
        // A binary string is already bytes, and this is the only function that
        // shows what one holds.
        ("SELECT HEX(X'C3A9')", "C3A9"),
        ("SELECT HEX(CAST('é' AS BINARY))", "C3A9"),
        // The numeric branch, which MySQL picks by the argument's declared
        // type. Zero is one character and a negative number is its two's
        // complement over 64 bits.
        ("SELECT HEX(255)", "FF"),
        ("SELECT HEX(0)", "0"),
        ("SELECT HEX(-1)", "FFFFFFFFFFFFFFFF"),
        ("SELECT HEX(-255)", "FFFFFFFFFFFFFF01"),
        ("SELECT HEX(TRUE)", "1"),
        ("SELECT HEX(18446744073709551615)", "FFFFFFFFFFFFFFFF"),
        ("SELECT HEX(9223372036854775808)", "8000000000000000"),
        // A DECIMAL rounds half away from zero.
        ("SELECT HEX(1.5)", "2"),
        ("SELECT HEX(1.4)", "1"),
        ("SELECT HEX(0.5)", "1"),
        ("SELECT HEX(2.5)", "3"),
        ("SELECT HEX(-2.5)", "FFFFFFFFFFFFFFFD"),
        ("SELECT HEX(-0.5)", "FFFFFFFFFFFFFFFF"),
        // A DOUBLE rounds half to *even*, which is a different answer for the
        // same three values: 2.5 goes down where the decimal went up.
        ("SELECT HEX(2.5e0)", "2"),
        ("SELECT HEX(3.5e0)", "4"),
        ("SELECT HEX(0.5e0)", "0"),
        ("SELECT HEX(-2.5e0)", "FFFFFFFFFFFFFFFE"),
        // A decimal too big for a BIGINT saturates silently where a double
        // raises 1690 below.
        (
            "SELECT HEX(CAST(1e30 AS DECIMAL(35,0)))",
            "7FFFFFFFFFFFFFFF",
        ),
        (
            "SELECT HEX(CAST(-1e30 AS DECIMAL(35,0)))",
            "8000000000000000",
        ),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // A double past the BIGINT range is 1690 rather than a saturated answer.
    // 8.3 answers the bare literal `HEX(1e30)` with `7FFFFFFFFFFFFFFF` and
    // every computed form of the same value with 1690, because the literal's
    // conversion is the unchecked one; this cannot tell the two apart and
    // takes the answer that is right for everything but the literal.
    let err = engine
        .execute(&mut session, "SELECT HEX(1e30 + 0)", &[])
        .unwrap_err();
    assert_eq!(err.code, 1690);

    // A temporal, an ENUM and a SET take the *string* branch, which is the
    // half of the split a value-shaped dispatch could get wrong: the same DATE
    // reads as the packed 20240105 in arithmetic and hexes as its characters.
    run(
        &engine,
        &mut session,
        "CREATE TABLE H (id INT NOT NULL, dt DATE NULL, e ENUM('a','b','c') NULL, \
         s SET('x','y','z') NULL, n INT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO H VALUES (1, '2024-01-05', 'b', 'x,z', 255)",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT HEX(dt), dt + 0, HEX(e), HEX(s), HEX(n) FROM H"
        ),
        vec![vec![
            "323032342D30312D3035",
            "20240105",
            "62",
            "782C7A",
            "FF"
        ]]
    );
}

#[test]
fn unhex_is_hex_backwards_and_null_for_anything_that_is_not_hex() {
    let (engine, mut session) = engine();

    for (sql, expected) in [
        ("SELECT HEX(UNHEX('616263'))", "616263"),
        // An odd-length string is read as if a zero nibble were written in
        // front of it, so this is one byte and not NULL and not 0xA0.
        ("SELECT HEX(UNHEX('a'))", "0A"),
        ("SELECT HEX(UNHEX('61626'))", "061626"),
        ("SELECT HEX(UNHEX('0'))", "00"),
        // Either case on the way in, and the same bytes out of both.
        ("SELECT HEX(UNHEX('c3a9'))", "C3A9"),
        ("SELECT HEX(UNHEX('C3A9'))", "C3A9"),
        // Bytes that are not text, which is the pair that says the answer is
        // held as bytes: a lone 0xFF or 0xC3 is not valid UTF-8, and a string
        // that had been through a lossy decode would hex as EFBFBD here.
        ("SELECT HEX(UNHEX('FF'))", "FF"),
        ("SELECT HEX(UNHEX('C3'))", "C3"),
        // The empty string is the empty string, not NULL.
        ("SELECT HEX(UNHEX(''))", ""),
        // Anything that is not a hex digit is NULL rather than skipped — a
        // space and a minus sign included.
        ("SELECT UNHEX('zz')", "NULL"),
        ("SELECT UNHEX('6g')", "NULL"),
        ("SELECT UNHEX('61 62')", "NULL"),
        ("SELECT UNHEX('-1')", "NULL"),
        ("SELECT UNHEX(NULL)", "NULL"),
        // No numeric branch at all: the number prints as characters and those
        // characters are read as hex.
        ("SELECT HEX(UNHEX(616263))", "616263"),
        ("SELECT HEX(UNHEX(0))", "00"),
        ("SELECT UNHEX(12.5)", "NULL"),
        ("SELECT UNHEX(-1)", "NULL"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }
}

#[test]
fn the_base64_pair_wraps_its_output_and_refuses_bad_input() {
    let (engine, mut session) = engine();

    for (sql, expected) in [
        ("SELECT TO_BASE64('abc')", "YWJj"),
        ("SELECT TO_BASE64('')", ""),
        ("SELECT TO_BASE64(NULL)", "NULL"),
        // The bytes of the value's *characters*, so a number encodes its
        // digits rather than its pattern — the opposite of `HEX`.
        ("SELECT TO_BASE64(255)", "MjU1"),
        ("SELECT TO_BASE64(-1)", "LTE="),
        ("SELECT TO_BASE64(12.5)", "MTIuNQ=="),
        ("SELECT TO_BASE64('é')", "w6k="),
        ("SELECT TO_BASE64(X'C3A9')", "w6k="),
        ("SELECT HEX(FROM_BASE64('YWJj'))", "616263"),
        ("SELECT HEX(FROM_BASE64('w6k='))", "C3A9"),
        ("SELECT HEX(FROM_BASE64('YQ=='))", "61"),
        ("SELECT HEX(FROM_BASE64('YWI='))", "6162"),
        ("SELECT HEX(FROM_BASE64(''))", ""),
        // Whitespace anywhere is skipped, which is what makes the round trip
        // survive the wrapping below.
        ("SELECT HEX(FROM_BASE64('YWJj YWJj'))", "616263616263"),
        // Everything else is NULL: a length that is not a multiple of four, a
        // character outside the alphabet, and an `=` anywhere but the end.
        ("SELECT FROM_BASE64('a')", "NULL"),
        ("SELECT FROM_BASE64('YWJ')", "NULL"),
        ("SELECT FROM_BASE64('!!!!')", "NULL"),
        ("SELECT FROM_BASE64('YWJj=')", "NULL"),
        ("SELECT FROM_BASE64('Y=Wj')", "NULL"),
        ("SELECT FROM_BASE64('=YWJ')", "NULL"),
        // A second padded group is refused as well, which earns an entry of
        // its own: MySQL's decoder has a flag for allowing one, and
        // `FROM_BASE64` looks like a caller that would pass it. Measured on
        // 8.3.0 — all three of these are NULL there, whitespace between the
        // groups or not.
        ("SELECT FROM_BASE64('YQ==YQ==')", "NULL"),
        ("SELECT FROM_BASE64('YQ== YQ==')", "NULL"),
        ("SELECT FROM_BASE64('YWI=YWJj')", "NULL"),
        ("SELECT FROM_BASE64(255)", "NULL"),
        ("SELECT FROM_BASE64(NULL)", "NULL"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // It wraps, which nothing about the name says. 57 bytes encode to exactly
    // 76 characters and get no newline at all; 58 bytes get one after the 76th
    // and so measure 81 rather than 80. Measured on 8.3.
    let a57 = "a".repeat(57);
    let a58 = "a".repeat(58);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            &format!("SELECT LENGTH(TO_BASE64('{a57}')), LENGTH(TO_BASE64('{a58}'))")
        ),
        vec![vec!["76", "81"]]
    );
    // The break is *between* lines rather than after each, so two full lines
    // carry one newline and not two: 114 bytes are 152 characters plus one.
    let a114 = "a".repeat(114);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            &format!("SELECT LENGTH(TO_BASE64('{a114}'))")
        ),
        vec![vec!["153"]]
    );
    // Where the break falls, which no length above can see: every length this
    // function produces is a multiple of four, and 76 and 77 give the same
    // count of them until the output is over fifteen hundred characters long.
    // The `0A` is the seventy-seventh character, and `HEX` is the only way to
    // ask where it is.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            &format!("SELECT HEX(TO_BASE64('{a58}'))")
        ),
        vec![vec![concat!(
            // Nineteen groups of `59574668` — the seventy-six characters of
            // the first line — and then the newline.
            "59574668595746685957466859574668595746685957466859574668595746685957466859574668",
            "595746685957466859574668595746685957466859574668595746685957466859574668",
            "0A59513D3D"
        )]]
    );

    // And the round trip closes over the wrapping: the newline the encoder put
    // in is one of the characters the decoder skips.
    let a100 = "a".repeat(100);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            &format!("SELECT FROM_BASE64(TO_BASE64('{a100}')) = '{a100}'")
        ),
        vec![vec!["1"]]
    );
}

#[test]
fn bin_reads_its_argument_through_the_characters_it_prints_as() {
    let (engine, mut session) = engine();

    for (sql, expected) in [
        ("SELECT BIN(12)", "1100"),
        ("SELECT BIN(0)", "0"),
        ("SELECT BIN(255)", "11111111"),
        ("SELECT BIN(TRUE)", "1"),
        ("SELECT BIN(NULL)", "NULL"),
        // Unsigned, so a negative number is the same sixty-four bits `HEX`
        // gives it and the top of the BIGINT UNSIGNED range survives.
        (
            "SELECT BIN(-1)",
            "1111111111111111111111111111111111111111111111111111111111111111",
        ),
        (
            "SELECT BIN(18446744073709551615)",
            "1111111111111111111111111111111111111111111111111111111111111111",
        ),
        // Through the characters, which is not the number the value carries:
        // the parse stops at the point where `HEX` rounded, and at the `e` of
        // an exponent.
        ("SELECT BIN(12.5)", "1100"),
        ("SELECT BIN(12.4)", "1100"),
        ("SELECT BIN(1e30)", "1"),
        ("SELECT BIN('12abc')", "1100"),
        ("SELECT BIN('abc12')", "0"),
        ("SELECT BIN('0x10')", "0"),
        ("SELECT BIN('010')", "1010"),
        ("SELECT BIN(' 12')", "1100"),
        ("SELECT BIN('+12')", "1100"),
        // The empty string alone is NULL. A space, and text with no digits in
        // it at all, are both zero.
        ("SELECT BIN('')", "NULL"),
        ("SELECT BIN(' ')", "0"),
        ("SELECT BIN('abc')", "0"),
        // The two directions saturate differently, which was measured rather
        // than derived.
        (
            "SELECT BIN('99999999999999999999999999')",
            "1111111111111111111111111111111111111111111111111111111111111111",
        ),
        ("SELECT BIN('-99999999999999999999999999')", "0"),
        ("SELECT BIN('-18446744073709551615')", "1"),
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec![expected]],
            "{sql}"
        );
    }

    // A temporal goes through its characters too, so this is 2024 and not the
    // packed 20240105 the same value reads as in arithmetic.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Bn (id INT NOT NULL, dt DATE NULL, b VARBINARY(9) NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Bn VALUES (1, '2024-01-05', 'abc')",
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT BIN(dt), dt + 0 FROM Bn"),
        vec![vec!["11111101000", "20240105"]]
    );

    // A binary string is refused rather than answered one of the two ways
    // MySQL answers one. `CONV` — which `BIN` is — takes the *number* a hex or
    // a bit literal spells, so `BIN(X'C3A9')` is 1100001110101001 on 8.3,
    // while the same bytes in a VARBINARY column are read as digits and give
    // 0. Both arrive as bytes here and nothing in the value says which.
    for sql in [
        "SELECT BIN(X'C3A9')",
        "SELECT BIN(CAST('abc' AS BINARY))",
        "SELECT BIN(b) FROM Bn",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1235, "expected a refusal for: {sql}");
    }
}

/// The half of these functions that is a fact about the column rather than
/// about the client, and the reason #0130 is worth more than five functions:
/// a column of bytes described as characters reaches Prisma as a String it
/// then mangles, and one of characters described as bytes as a Buffer it
/// refuses to convert.
///
/// Measured on 8.3: `UNHEX(x)` and `FROM_BASE64(x)` are `cs=63 flags=[BINARY]`
/// and `HEX(x)`, `TO_BASE64(x)` and `BIN(x)` are `cs=45 flags=[]`.
#[test]
fn unhex_and_from_base64_describe_their_answers_as_bytes() {
    let (engine, mut session) = engine();

    let bytes = |session: &Session, sql: &str| -> Vec<bool> {
        engine
            .describe(session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| {
                let wire = c.to_wire();
                assert_eq!(
                    wire.charset == mysql_ultralight_protocol::column::charset::BINARY,
                    wire.flags & mysql_ultralight_protocol::column::flags::BINARY != 0,
                    "the charset and the flag have to agree, for: {sql}"
                );
                wire.charset == mysql_ultralight_protocol::column::charset::BINARY
            })
            .collect()
    };

    assert_eq!(
        bytes(
            &session,
            "SELECT HEX('abc'), UNHEX('4142'), BIN(12), TO_BASE64('abc'), FROM_BASE64('YWJj')"
        ),
        vec![false, true, false, false, true]
    );
    // Read off the expression and not off the value, so a NULL answer is still
    // described as bytes — which is what 8.3 does, and what a client sizing a
    // buffer from the metadata needs before any row arrives.
    assert_eq!(
        bytes(&session, "SELECT UNHEX(NULL), FROM_BASE64(NULL), HEX(NULL)"),
        vec![true, true, false]
    );

    // Over a table, which the engine plans through a different projection from
    // the bare literals above. The two paths set this field separately, so a
    // test that only asked one of them would pass with the other unwired.
    run(
        &engine,
        &mut session,
        "CREATE TABLE Bin (id INT NOT NULL, v VARCHAR(20) NULL, PRIMARY KEY (id))",
    );
    assert_eq!(
        bytes(&session, "SELECT UNHEX(v), FROM_BASE64(v), HEX(v) FROM Bin"),
        vec![true, true, false]
    );
    // And through a group, which is a third path. The window one is the fourth
    // and is unreachable by construction — neither of these is a window
    // function — so it is set there for the same reason its neighbours are and
    // has nothing to prove.
    assert_eq!(
        bytes(
            &session,
            "SELECT UNHEX(v), FROM_BASE64(v), HEX(v) FROM Bin GROUP BY v"
        ),
        vec![true, true, false]
    );

    // The four above are the projection paths. This is the fifth, and it is a
    // different mechanism — which is why enumerating the four was not enough:
    // a derived table, a CTE and a view do not re-run `binary_answer` at all.
    // They rebuild the column from its `SqlType` through
    // `OutColumn::from_sql_type`, so the widening in `OutColumn::sql_type` is
    // the only thing that can carry this, and while it could not,
    // `SELECT x FROM (SELECT UNHEX('4142') AS x) d` came back a utf8mb4 column
    // still carrying bytes — the mangling these functions exist to stop. All
    // of these are `cs=63 flags=[BINARY]` on 8.3.
    for sql in [
        "SELECT x FROM (SELECT UNHEX('4142') AS x) d",
        "WITH c AS (SELECT UNHEX('4142') AS x) SELECT x FROM c",
        "SELECT x FROM (SELECT FROM_BASE64('YWJj') AS x) d",
        "SELECT UNHEX('4142') AS x UNION ALL SELECT UNHEX('4344')",
    ] {
        assert_eq!(bytes(&session, sql), vec![true], "{sql}");
    }
    // And the text half of each pair still travels as text through the same
    // path, so the widening did not simply turn everything binary.
    for sql in [
        "SELECT x FROM (SELECT HEX('ab') AS x) d",
        "WITH c AS (SELECT TO_BASE64('ab') AS x) SELECT x FROM c",
    ] {
        assert_eq!(bytes(&session, sql), vec![false], "{sql}");
    }
    // `varbinary` and not `binary` or `blob`, which the charset alone cannot
    // say: all three carry the binary character set and only `varbinary`
    // leaves the wire type a VAR_STRING. 8.3 sends VAR_STRING for every one of
    // the statements above, so getting the spelling wrong would announce a
    // `MYSQL_TYPE_BLOB` or a `MYSQL_TYPE_STRING` under a green charset check.
    let code = |session: &Session, sql: &str| -> Vec<String> {
        engine
            .describe(session, sql, 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| format!("{:?}", c.ty))
            .collect()
    };
    for sql in [
        "SELECT x FROM (SELECT UNHEX('4142') AS x) d",
        "WITH c AS (SELECT FROM_BASE64('YWJj') AS x) SELECT x FROM c",
        "SELECT UNHEX('4142') AS x UNION ALL SELECT UNHEX('4344')",
    ] {
        assert_eq!(code(&session, sql), vec!["VarString"], "{sql}");
    }

    // The rows survive it too: a byte that is not UTF-8 has to come back out
    // of a derived table as the byte it went in as.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT HEX(x) FROM (SELECT UNHEX('FF') AS x) d"
        ),
        vec![vec!["FF"]]
    );
}

// ---------------------------------------------------------------- names

/// A name is what was written between the quotes, not the rendering of it.
///
/// Read through `Display` and trimmed back, an identifier loses two different
/// things, and the engine lost both. `Display` *doubles* an embedded backtick
/// and trimming the outer pair cannot undo it; and a caller that splits the
/// rendering on `.` cannot tell a qualifier from a dot inside the name.
///
/// Measured on 8.3, which accepts every one of these and reports the value:
///
/// | written | 8.3 `information_schema` | 8.3 `SHOW CREATE TABLE` |
/// |---|---|---|
/// | `` `we``ird` `` | ``we`ird`` | `` CREATE TABLE `we``ird` `` |
/// | `` `my.table` `` | `my.table` | `` CREATE TABLE `my.table` `` |
/// | `` `select` `` | `select` | `` CREATE TABLE `select` `` |
/// | `` `tabké` `` | `tabké` | `` CREATE TABLE `tabké` `` |
///
/// The ones it refuses are refused for the name and not the quoting: a
/// trailing space is 1103/1166 and so is an empty one, whatever they are
/// wrapped in.
#[test]
fn a_quoted_name_keeps_the_value_it_was_written_with() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE `we``ird` (`c``ol` INT)",
    );
    run(
        &engine,
        &mut session,
        "CREATE INDEX `ix``x` ON `we``ird` (`c``ol`)",
    );
    run(&engine, &mut session, "INSERT INTO `we``ird` VALUES (7)");

    // One backtick in every reported name, not the two the DDL was written
    // with. The column was already right, which is what makes the table name
    // beside it evidence rather than a constant.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = 'testdb' ORDER BY COLUMN_NAME",
        ),
        vec![vec!["we`ird", "c`ol"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA = 'testdb'",
        ),
        vec![vec!["we`ird"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT INDEX_NAME, COLUMN_NAME FROM information_schema.STATISTICS \
             WHERE TABLE_SCHEMA = 'testdb'",
        ),
        vec![vec!["ix`x", "c`ol"]]
    );
    // And on the wire, where an alias is the name the client reads back.
    assert_eq!(
        engine
            .describe(&session, "SELECT `c``ol` AS `al``ias` FROM `we``ird`", 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>(),
        vec!["al`ias"]
    );

    // An assignment target and a USING list read a name through paths of their
    // own, and each one that still went through `Display` would find no such
    // column — the metadata above cannot reach either.
    assert_eq!(
        affected(&engine, &mut session, "UPDATE `we``ird` SET `c``ol` = 8"),
        1
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT `c``ol` FROM `we``ird`"),
        vec![vec!["8"]]
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE `oth``er` (`c``ol` INT)",
    );
    run(&engine, &mut session, "INSERT INTO `oth``er` VALUES (8)");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT o.`c``ol` FROM `we``ird` w JOIN `oth``er` o USING (`c``ol`)",
        ),
        vec![vec!["8"]]
    );
    run(&engine, &mut session, "DROP INDEX `ix``x` ON `we``ird`");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = 'testdb'",
        ),
        vec![vec!["0"]]
    );

    // A name only a keyword, and a non-ASCII one: both are values like any
    // other and neither may be transformed on the way to being reported.
    run(&engine, &mut session, "CREATE TABLE `select` (`from` INT)");
    run(&engine, &mut session, "CREATE TABLE `tabké` (`colé` INT)");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME IN ('select', 'tabké') \
             ORDER BY TABLE_NAME",
        ),
        vec![vec!["select", "from"], vec!["tabké", "colé"]]
    );
}

/// Every statement addresses one dotted name the same way.
///
/// `INSERT` read its table through `insert.table.to_string()` and split the
/// rendering on `.`, where every other path reads the name part by part. So
/// `CREATE TABLE` and `SELECT` were happy with `` `my.table` `` and the
/// `INSERT` beside them was 1049 `Unknown database 'my'` — the engine
/// disagreeing with itself about one name, against an 8.3 that inserts the
/// row.
#[test]
fn a_dotted_name_is_one_table_to_every_statement() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE `my.table` (a INT)");
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "INSERT INTO `my.table` (a) VALUES (1)"
        ),
        1
    );
    assert_eq!(
        affected(&engine, &mut session, "INSERT INTO `my.table` VALUES (2)"),
        1
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT a FROM `my.table` ORDER BY a"),
        vec![vec!["1"], vec!["2"]]
    );
    // The writes, which is where the split lived, and the DDL that renames an
    // index — the one other path that took a name as a rendered string.
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "UPDATE `my.table` SET a = 3 WHERE a = 1"
        ),
        1
    );
    assert_eq!(
        affected(&engine, &mut session, "DELETE FROM `my.table` WHERE a = 3"),
        1
    );
    run(&engine, &mut session, "CREATE INDEX ixa ON `my.table` (a)");
    run(
        &engine,
        &mut session,
        "ALTER TABLE `my.table` RENAME INDEX ixa TO ixb",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TABLE_NAME, INDEX_NAME FROM information_schema.STATISTICS \
             WHERE TABLE_SCHEMA = 'testdb'",
        ),
        vec![vec!["my.table", "ixb"]]
    );
    // A real qualifier still qualifies, so the fix did not simply stop
    // splitting: this one names the database and 8.3 reads it the same way.
    assert_eq!(
        rows(&engine, &mut session, "SELECT a FROM testdb.`my.table`"),
        vec![vec!["2"]]
    );
    // And the arm that refuses a deeper name still names it. The joined
    // spelling is now built only here — every other call skips it — so a
    // regression would be a message with nothing in it rather than a failure.
    let err = engine
        .execute(&mut session, "SELECT a FROM a.b.c", &[])
        .unwrap_err();
    assert!(err.to_string().contains("qualified name `a.b.c`"), "{err}");
}

/// `COLLATE` reports the collation, not the spelling of the collation.
///
/// The DDL stored `ColumnOption::Collation` through `to_string()` on an
/// `ObjectName`, so `` COLLATE `utf8mb4_general_ci` `` reached
/// `information_schema.COLUMNS.COLLATION_NAME` — and the state image — with
/// its backticks still on. 8.3 reports `utf8mb4_general_ci` for every spelling
/// below, the upper-case one included: it lowercases the name it records.
#[test]
fn a_collation_is_named_the_same_however_it_is_written() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE C (a VARCHAR(10) COLLATE `utf8mb4_general_ci`, \
                         b VARCHAR(10) COLLATE utf8mb4_general_ci, \
                         c VARCHAR(10) COLLATE 'utf8mb4_general_ci', \
                         d VARCHAR(10) COLLATE UTF8MB4_GENERAL_CI)",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'C' ORDER BY COLUMN_NAME",
        ),
        vec![
            vec!["a", "utf8mb4_general_ci"],
            vec!["b", "utf8mb4_general_ci"],
            vec!["c", "utf8mb4_general_ci"],
            vec!["d", "utf8mb4_general_ci"],
        ]
    );

    // The expression form reads the same name through the same reader, so a
    // quoted `_ci` collation is honoured there too.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT 'a' COLLATE `utf8mb4_general_ci`"
        ),
        vec![vec!["a"]]
    );
}

/// A collation this engine cannot honour is refused, in the DDL as well as in
/// an expression.
///
/// The DDL recorded any collation at all and then compared the column the way
/// it compares every other one, which is a wrong answer in two directions.
/// Both measured on 8.3, against `VARCHAR(10) COLLATE utf8mb4_bin` under a
/// UNIQUE key: it stores `'a'` and `'A'` side by side where this engine
/// answered 1062, and `WHERE a = 'A'` returns `A` there and returned `a` here.
///
/// So the two paths now share one reader. Refusing is the direction that
/// cannot be silently wrong, and it turns nothing real away — the conformance
/// target spells all 314 of its `COLLATE` clauses `_ci`.
#[test]
fn a_collation_the_engine_cannot_honour_is_refused() {
    let (engine, mut session) = engine();
    for sql in [
        "CREATE TABLE B (a VARCHAR(10) COLLATE utf8mb4_bin)",
        "CREATE TABLE B (a VARCHAR(10) COLLATE `utf8mb4_bin`)",
        "CREATE TABLE B (a VARCHAR(10) COLLATE utf8mb4_0900_as_cs)",
        "CREATE TABLE B (a VARCHAR(10)) COLLATE=utf8mb4_bin",
        "CREATE TABLE B (a VARCHAR(10)) DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin",
        "SELECT 'a' COLLATE utf8mb4_bin",
        "SELECT 'a' COLLATE `utf8mb4_bin`",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        // Never 1273: these are collations 8.3 has, so calling them unknown
        // would be a false claim about the name rather than a report of this
        // engine's gap.
        assert_ne!(err.code, 1273, "{sql} -> {err}");
        // Named without its backticks, whichever path refused it: the message
        // has to name the collation, not the rendering it was written with.
        assert!(
            err.to_string().contains("`utf8mb4_bin` collation")
                || err.to_string().contains("`utf8mb4_0900_as_cs` collation"),
            "{sql} -> {err}"
        );
    }
    // Refused, not half-applied: no table is left behind.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = 'testdb'",
        ),
        vec![vec!["0"]]
    );
}

/// A name that is not a collation at all is 1273, as 8.3 answers it.
///
/// The suffix check alone blessed anything ending `_ci`, so `COLLATE bogus_ci`
/// succeeded — and was lowercased, written into the append-only state image
/// and reported to whatever introspected the schema afterwards. Measured: 8.3
/// answers `ERROR 1273 (HY000): Unknown collation: 'bogus_ci'`, at the column
/// level and at the table level alike.
#[test]
fn a_name_that_is_not_a_collation_is_unknown() {
    let (engine, mut session) = engine();
    for sql in [
        "CREATE TABLE B (a VARCHAR(10) COLLATE bogus_ci)",
        "CREATE TABLE B (a VARCHAR(10) COLLATE `bogus_ci`)",
        "CREATE TABLE B (a VARCHAR(10)) COLLATE=bogus_ci",
        "SELECT 'a' COLLATE bogus_ci",
    ] {
        let err = engine.execute(&mut session, sql, &[]).unwrap_err();
        assert_eq!(err.code, 1273, "{sql} -> {err}");
        assert_eq!(err.sql_state, "HY000", "{sql} -> {err}");
        assert!(
            err.to_string().contains("Unknown collation: \'bogus_ci\'"),
            "{sql} -> {err}"
        );
    }
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = 'testdb'",
        ),
        vec![vec!["0"]]
    );
}

/// What `information_schema.COLLATIONS` lists and what a `COLLATE` clause
/// accepts are one list.
///
/// A server that advertises a collation and then refuses it — or refuses one
/// it advertises as unknown — is contradicting itself about its own
/// capabilities, and these were two hand-written lists. The clause was checked
/// for a `_ci` suffix alone, so it accepted names the table did not list at
/// all.
#[test]
fn the_collations_table_and_the_collate_clause_agree() {
    let (engine, mut session) = engine();
    let listed = rows(
        &engine,
        &mut session,
        "SELECT COLLATION_NAME FROM information_schema.COLLATIONS ORDER BY COLLATION_NAME",
    );
    assert!(!listed.is_empty(), "the table should list something");

    for row in &listed {
        let name = &row[0];
        let sql = format!("SELECT 'a' COLLATE `{name}`");
        match engine.execute(&mut session, &sql, &[]) {
            // Advertised and honoured.
            Ok(_) => {}
            // Advertised and not honourable is allowed — `binary` is — but it
            // may never come back as *unknown*, which is what the table would
            // be contradicting.
            Err(e) => assert_ne!(e.code, 1273, "{sql} -> {e}"),
        }
    }

    // And the other direction: a name the table does not list is unknown,
    // rather than being waved through on the strength of its suffix.
    let absent = "definitely_not_a_collation_ci";
    assert!(
        !listed.iter().any(|r| r[0] == absent),
        "the fixture name must not be listed"
    );
    let err = engine
        .execute(&mut session, &format!("SELECT 'a' COLLATE `{absent}`"), &[])
        .unwrap_err();
    assert_eq!(err.code, 1273, "{err}");
}

/// A table-level `COLLATE` is the clause the column-level one was refused for,
/// written one line higher.
///
/// It was parsed and dropped — `create.table_options` was never read — so the
/// table reported the database's collation and compared its columns the way
/// every other one is compared. Measured on 8.3: the clause sets
/// `TABLE_COLLATION` and shows up on every *character* column of the table,
/// and on none of the others.
#[test]
fn a_table_level_collation_reaches_the_columns_it_covers() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (a VARCHAR(10), b INT, c TEXT COLLATE utf8mb4_general_ci) \
         DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TABLE_COLLATION FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'T'",
        ),
        vec![vec!["utf8mb4_unicode_ci"]]
    );
    // The table's collation on the character columns that declare none, the
    // column's own where it declares one, and NULL on the INT.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'T' ORDER BY ORDINAL_POSITION",
        ),
        vec![
            vec!["a", "utf8mb4_unicode_ci"],
            vec!["b", "NULL"],
            vec!["c", "utf8mb4_general_ci"],
        ]
    );
}

/// A view's column carries the collation of the column behind it.
///
/// The shape was built with no collation at all, so a view over a column with
/// an explicit `COLLATE` reported the database default. 8.3 reports the base
/// column's, and NULL for a column that holds no characters.
#[test]
fn a_view_reports_the_collation_of_the_column_behind_it() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Base (a VARCHAR(10) COLLATE utf8mb4_general_ci, n INT)",
    );
    run(
        &engine,
        &mut session,
        "CREATE VIEW V AS SELECT a, n FROM Base",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'V' ORDER BY ORDINAL_POSITION",
        ),
        vec![vec!["a", "utf8mb4_general_ci"], vec!["n", "NULL"]]
    );
}

/// One column may not describe itself two ways.
///
/// `CREATE TABLE ... AS SELECT COLUMN_TYPE FROM information_schema.COLUMNS`
/// copies a column this engine marks `binary_collation`, which is what puts
/// the BINARY flag on its wire definition — and `COLLATION_NAME` beside it
/// reported the database's `utf8mb4_0900_ai_ci`. 8.3 reports `utf8mb3_bin`,
/// which is the collation the flag stands for.
#[test]
fn a_binary_collated_column_reports_the_collation_its_wire_flag_means() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Copied AS SELECT COLUMN_TYPE, COLUMN_NAME \
         FROM information_schema.columns WHERE TABLE_NAME = 'Copied'",
    );
    let binary_flags: Vec<bool> = engine
        .describe(&session, "SELECT * FROM Copied", 0)
        .unwrap()
        .unwrap()
        .iter()
        .map(|c| c.to_wire().flags & mysql_ultralight_protocol::column::flags::BINARY != 0)
        .collect();
    // The pair is the claim: the first column carries the flag and the second
    // does not, and what `information_schema` says has to agree with both.
    assert_eq!(binary_flags, vec![true, false]);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'Copied' ORDER BY ORDINAL_POSITION",
        ),
        vec![
            vec!["COLUMN_TYPE", "utf8mb3_bin"],
            vec!["COLUMN_NAME", "utf8mb4_0900_ai_ci"],
        ]
    );
}

/// Only a column that holds characters has a collation.
///
/// Every column of every type reported the database's collation, so an `INT`
/// claimed `utf8mb4_0900_ai_ci`. Measured on 8.3 across all twelve types
/// below: `char`/`varchar`/`text`, `enum` and `set` report one and the rest —
/// the numbers, the temporals, `json`, and the byte types `blob`, `binary`
/// and `varbinary` — report NULL.
#[test]
fn only_a_character_column_reports_a_collation() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Ty (i INT, d DATE, dt DATETIME(3), f DOUBLE, n DECIMAL(10,2), \
                          t TEXT, v VARCHAR(10), b BLOB, vb VARBINARY(10), \
                          e ENUM('x','y'), st SET('p','q'), j JSON)",
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'Ty' ORDER BY ORDINAL_POSITION",
        ),
        vec![
            vec!["i", "NULL"],
            vec!["d", "NULL"],
            vec!["dt", "NULL"],
            vec!["f", "NULL"],
            vec!["n", "NULL"],
            vec!["t", "utf8mb4_0900_ai_ci"],
            vec!["v", "utf8mb4_0900_ai_ci"],
            vec!["b", "NULL"],
            vec!["vb", "NULL"],
            vec!["e", "utf8mb4_0900_ai_ci"],
            vec!["st", "utf8mb4_0900_ai_ci"],
            vec!["j", "NULL"],
        ]
    );
}

/// A foreign key's error message quotes its names back as SQL.
///
/// The message is not reporting names, it is quoting SQL — 8.3 writes
/// ``REFERENCES `pa``rent` `` with the backtick doubled, exactly as
/// `SHOW CREATE TABLE` does. These sites wrapped each name in backticks
/// without escaping it, which was right only for as long as the name reaching
/// them still carried its own doubling; reading the value correctly broke
/// them, so the re-quoting is now explicit and `name::quote` is the one place
/// it happens.
///
/// The whole message is asserted rather than a fragment: the constraint, the
/// child column and the parent table each go through it separately, and an
/// assertion on one would pass while the other two were unescaped.
#[test]
fn a_foreign_key_violation_quotes_every_name_it_writes() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE `pa``rent` (id INT NOT NULL PRIMARY KEY)",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE `ch``ild` (id INT NOT NULL PRIMARY KEY, `pi``d` INT NULL, \
         CONSTRAINT `fk``x` FOREIGN KEY (`pi``d`) REFERENCES `pa``rent` (id))",
    );

    let err = engine
        .execute(&mut session, "INSERT INTO `ch``ild` VALUES (1, 99)", &[])
        .unwrap_err();
    // 8.3, character for character, but for the child table: it writes
    // ``  `testdb`.`ch``ild`  `` and this engine does not schema-qualify it,
    // which is a gap of its own and not this one.
    assert_eq!(
        err.to_string(),
        "ERROR 1452 (23000): Cannot add or update a child row: a foreign key constraint \
         fails (`ch``ild`, CONSTRAINT `fk``x` FOREIGN KEY (`pi``d`) \
         REFERENCES `pa``rent` (`id`))"
    );

    // The 1451 half is a separate format string and was wrong the same way.
    run(&engine, &mut session, "INSERT INTO `pa``rent` VALUES (1)");
    run(&engine, &mut session, "INSERT INTO `ch``ild` VALUES (2, 1)");
    let err = engine
        .execute(&mut session, "DELETE FROM `pa``rent` WHERE id = 1", &[])
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "ERROR 1451 (23000): Cannot delete or update a parent row: a foreign key constraint \
         fails (`ch``ild`, CONSTRAINT `fk``x` FOREIGN KEY (`pi``d`) \
         REFERENCES `pa``rent` (`id`))"
    );
}

/// A qualified column name in an INSERT's column list names the column.
///
/// `INSERT INTO q (q.a, q.b) VALUES (1, 2)` is legal on 8.3 — measured, and
/// `INSERT INTO db.q (db.q.a)` with it. Matching on every part joined looks
/// for a column called `q.a`, which no table has, so this was 1054.
#[test]
fn an_insert_column_list_takes_the_column_out_of_a_qualified_name() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE TABLE q (a INT, b INT)");
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "INSERT INTO q (q.a, q.b) VALUES (1, 2)"
        ),
        1
    );
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "INSERT INTO testdb.q (testdb.q.a) VALUES (5)"
        ),
        1
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT a, b FROM q ORDER BY a"),
        vec![vec!["1", "2"], vec!["5", "NULL"]]
    );
}

/// A prepared statement's source is the literal's value, not the text between
/// the first and last quote of it.
///
/// `trim_matches('\'')` eats the doubling at the end of the statement as if it
/// were part of the closing quote, so this prepared `SELECT ''x` and failed to
/// parse. 8.3 answers `x`.
#[test]
fn a_prepared_statement_undoubles_the_quotes_in_its_source() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "PREPARE s FROM 'SELECT ''x'' AS c'");
    assert_eq!(rows(&engine, &mut session, "EXECUTE s"), vec![vec!["x"]]);
}

/// `USE` takes the database's name out of the tree.
///
/// It was read out of `format!("{use_stmt}")` with the rendered `USE ` stripped
/// back off the front, which put a quoted name into the session under its
/// rendering. 8.3 accepts both of these and `DATABASE()` reports the value.
#[test]
fn use_selects_the_database_it_names() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "CREATE DATABASE `db``x`");
    run(&engine, &mut session, "CREATE DATABASE `my.db`");

    run(&engine, &mut session, "USE `db``x`");
    assert_eq!(
        rows(&engine, &mut session, "SELECT DATABASE()"),
        vec![vec!["db`x"]]
    );
    run(&engine, &mut session, "CREATE TABLE t (a INT)");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT TABLE_SCHEMA FROM information_schema.TABLES WHERE TABLE_NAME = 't'",
        ),
        vec![vec!["db`x"]]
    );

    run(&engine, &mut session, "USE `my.db`");
    assert_eq!(
        rows(&engine, &mut session, "SELECT DATABASE()"),
        vec![vec!["my.db"]]
    );
}

/// The table the ORDER BY resolution tests below read.
///
/// `id` and `b` run in opposite directions on purpose: every statement here
/// answers `1,2,3,4` when it sorts on the one it should and `4,3,2,1` when it
/// sorts on the other, so a test that reads the rows cannot pass with the two
/// resolutions swapped. A table where they agreed would have proved nothing.
fn order_rows(engine: &Engine, session: &mut Session) {
    run(
        engine,
        session,
        "CREATE TABLE O (id INT NOT NULL, b INT NOT NULL, s VARCHAR(10) NOT NULL, PRIMARY KEY (id))",
    );
    run(
        engine,
        session,
        "INSERT INTO O VALUES (1,4,'x'),(2,3,'y'),(3,2,'z'),(4,1,'w')",
    );
}

/// The error a statement is refused with, so a test can name the code.
fn refusal(
    engine: &Engine,
    session: &mut Session,
    sql: &str,
) -> mysql_ultralight_engine::EngineError {
    engine
        .execute(session, sql, &[])
        .err()
        .unwrap_or_else(|| panic!("expected a refusal from: {sql}"))
}

/// `ORDER BY <position>` counts over the select list, not over the relation.
///
/// #0090. `SELECT JSON_ARRAY(x) FROM (SELECT 1 AS x UNION SELECT 0) t ORDER BY
/// 1` answers `[0]` then `[1]` on 8.3 — the position names the document the
/// query returns. This engine evaluated the number against the source row,
/// where position 1 is `x`, and answered `[1]` then `[0]`: every row present
/// and plausibly ordered, which is what made it quiet.
#[test]
fn an_order_by_position_names_the_select_list() {
    let (engine, mut session) = engine();
    order_rows(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT JSON_ARRAY(x) FROM (SELECT 1 AS x UNION SELECT 0) t ORDER BY 1"
        ),
        vec![vec!["[0]"], vec!["[1]"]]
    );
    // The second output column and not the second column of `O`, which are
    // different columns here and sort opposite ways.
    assert_eq!(
        rows(&engine, &mut session, "SELECT s, id FROM O ORDER BY 2"),
        vec![
            vec!["x", "1"],
            vec!["y", "2"],
            vec!["z", "3"],
            vec!["w", "4"]
        ]
    );
    // A `*` is expanded before positions are counted, or `ORDER BY 3` would
    // have no third column to name.
    assert_eq!(
        rows(&engine, &mut session, "SELECT * FROM O ORDER BY 3"),
        vec![
            vec!["4", "1", "w"],
            vec!["1", "4", "x"],
            vec!["2", "3", "y"],
            vec!["3", "2", "z"]
        ]
    );
    // Only a bare integer literal is a position. Parentheses come off — 8.3
    // sorts `ORDER BY (2)` on the second column — but arithmetic does not:
    // `2 + 0` is the constant two and orders nothing, which is why this one
    // comes back in insertion order.
    assert_eq!(
        rows(&engine, &mut session, "SELECT s, id FROM O ORDER BY (2)"),
        vec![
            vec!["x", "1"],
            vec!["y", "2"],
            vec!["z", "3"],
            vec!["w", "4"]
        ]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT s, id FROM O ORDER BY 2 + 0"),
        vec![
            vec!["x", "1"],
            vec!["y", "2"],
            vec!["z", "3"],
            vec!["w", "4"]
        ]
    );
}

/// A position the select list cannot answer is 1054 under this clause's name.
///
/// Both ends, because both are positions on 8.3 rather than constants:
/// `ORDER BY 5` over one select item and `ORDER BY 0` are `Unknown column '5'`
/// and `'0'` `in 'order clause'`. This engine sorted every row by the same
/// constant and answered.
///
/// Over no rows as well as over some, because the position used to be read
/// while a row was being keyed: an empty result never reached it.
#[test]
fn an_order_by_position_that_names_nothing_is_refused() {
    let (engine, mut session) = engine();
    order_rows(&engine, &mut session);

    for (sql, name) in [
        ("SELECT id FROM O ORDER BY 5", "5"),
        ("SELECT id FROM O ORDER BY 0", "0"),
        ("SELECT id FROM O ORDER BY b, 5", "5"),
        ("SELECT * FROM O ORDER BY 4", "4"),
        ("SELECT id FROM O WHERE 0 ORDER BY 5", "5"),
        ("SELECT DISTINCT id FROM O ORDER BY 2", "2"),
        ("SELECT id, COUNT(*) FROM O GROUP BY id ORDER BY 5", "5"),
        ("SELECT 1 AS one ORDER BY 2", "2"),
        (
            "SELECT id FROM O WHERE 0 UNION SELECT b FROM O WHERE 0 ORDER BY 5",
            "5",
        ),
    ] {
        let err = refusal(&engine, &mut session, sql);
        assert_eq!(err.code, 1054, "expected ER_BAD_FIELD_ERROR for: {sql}");
        assert_eq!(
            err.message,
            format!("Unknown column '{name}' in 'order clause'"),
            "for: {sql}"
        );
    }
}

/// A bare name sees the select list first; a name inside an expression sees
/// the FROM clause first.
///
/// #0144, and the precedence rather than a choice of one over the other. Both
/// directions measured on 8.3 over a table with a column `b`:
///
/// ```sql
/// SELECT id AS b FROM O ORDER BY b       -- the alias:  1,2,3,4
/// SELECT id AS b FROM O ORDER BY b + 1   -- the column: 4,3,2,1
/// SELECT id AS zz FROM O ORDER BY zz + 1 -- the alias, since `zz` is no column
/// ```
///
/// Swapping the two would answer half of these backwards, which is why they
/// are one test: this engine sorted before the projection existed, so `b`
/// found the column both times.
#[test]
fn an_order_by_name_prefers_the_select_list_and_falls_back_to_the_relation() {
    let (engine, mut session) = engine();
    order_rows(&engine, &mut session);

    let ids = |engine: &Engine, session: &mut Session, sql: &str| -> Vec<String> {
        rows(engine, session, sql)
            .into_iter()
            .map(|row| row[0].clone())
            .collect()
    };

    // The alias, over a column of the same name.
    assert_eq!(
        ids(&engine, &mut session, "SELECT id AS b FROM O ORDER BY b"),
        ["1", "2", "3", "4"]
    );
    // Parentheses do not make it an expression: 8.3 reads `(b)` as the bare
    // name it wraps.
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT id AS b FROM O ORDER BY ((b))"
        ),
        ["1", "2", "3", "4"]
    );
    // The column, because the name is inside an expression.
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT id AS b FROM O ORDER BY b + 1"
        ),
        ["4", "3", "2", "1"]
    );
    assert_eq!(
        ids(&engine, &mut session, "SELECT id AS b FROM O ORDER BY -b"),
        ["1", "2", "3", "4"]
    );
    // The alias even inside an expression, because the relation has no such
    // column to prefer.
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT id AS zz FROM O ORDER BY zz + 1"
        ),
        ["1", "2", "3", "4"]
    );
    // A qualifier never reaches an alias, at either level.
    assert_eq!(
        ids(&engine, &mut session, "SELECT id AS b FROM O ORDER BY O.b"),
        ["4", "3", "2", "1"]
    );
    // An alias inside an expression `substitute_names` does not descend into
    // reaches the sort unsubstituted and is retried against the projected
    // result, which is how this engine has always answered it. The resolution
    // has to leave that path alone rather than refuse the name it could not
    // place.
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT id AS zz FROM O ORDER BY ABS(zz)"
        ),
        ["1", "2", "3", "4"]
    );
    // The thing the old comment was right about, which the fix had to keep:
    // ORDER BY may name a column the select list does not return.
    assert_eq!(
        ids(&engine, &mut session, "SELECT id FROM O ORDER BY b"),
        ["4", "3", "2", "1"]
    );
    // An alias that shadows the column it is derived from resolves to the same
    // value either way, and has to keep answering.
    assert_eq!(
        ids(&engine, &mut session, "SELECT id AS id FROM O ORDER BY id"),
        ["1", "2", "3", "4"]
    );
    // A join, where the shadowed column belongs to the other table.
    run(
        &engine,
        &mut session,
        "CREATE TABLE O2 (id INT NOT NULL, c INT NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO O2 VALUES (1,40),(2,30),(3,20),(4,10)",
    );
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT O.id AS c FROM O JOIN O2 ON O2.id = O.id ORDER BY c"
        ),
        ["1", "2", "3", "4"]
    );
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT O.id AS c FROM O JOIN O2 ON O2.id = O.id ORDER BY c + 1"
        ),
        ["4", "3", "2", "1"]
    );
    // A `USING` column is one column in MySQL and two in this engine's
    // relation, so the two same-named items a `*` puts in front of the clause
    // would otherwise be ambiguous and refuse a query 8.3 answers.
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT * FROM O JOIN O2 USING (id) ORDER BY id"
        ),
        ["1", "2", "3", "4"]
    );
    // And it may only reach the columns this wildcard expanded. Searching
    // everything projected so far bound the second `id` to the aliased
    // `LENGTH(O.s)` in front of it, which is constant here — so the rows came
    // back in scan order where 8.3 sorts them by `O.id`. Read at the second
    // column, because the first is that constant either way.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT LENGTH(O.s) AS id, O.*, O2.* FROM O JOIN O2 USING (id) ORDER BY 2 DESC"
        )
        .into_iter()
        .map(|row| row[1].clone())
        .collect::<Vec<_>>(),
        ["4", "3", "2", "1"]
    );
}

/// The same precedence on the grouped path.
///
/// It had the opposite bug from the ungrouped one: it substituted an alias for
/// every name at any depth, so `ORDER BY b + 0` sorted on the alias where 8.3
/// sorts on the grouped column. One resolution now serves both paths, which is
/// the only way the two can be held to the same rule.
#[test]
fn a_grouped_order_by_resolves_the_same_way() {
    let (engine, mut session) = engine();
    order_rows(&engine, &mut session);

    let ids = |engine: &Engine, session: &mut Session, sql: &str| -> Vec<String> {
        rows(engine, session, sql)
            .into_iter()
            .map(|row| row[0].clone())
            .collect()
    };

    // Each group holds one row, so `MAX(id)` is that row's `id` and the two
    // readings of `b` order the four groups opposite ways.
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT MAX(id) AS b FROM O GROUP BY b ORDER BY b"
        ),
        ["1", "2", "3", "4"]
    );
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT MAX(id) AS b FROM O GROUP BY b ORDER BY b + 0"
        ),
        ["4", "3", "2", "1"]
    );
    // A position still counts over the select list, and still binds the value
    // computed for it rather than the expression behind it.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT s, COUNT(*) AS n FROM O GROUP BY s ORDER BY 1"
        ),
        vec![
            vec!["w", "1"],
            vec!["x", "1"],
            vec!["y", "1"],
            vec!["z", "1"]
        ]
    );
    let err = refusal(
        &engine,
        &mut session,
        "SELECT MAX(id) AS b FROM O GROUP BY b ORDER BY 9",
    );
    assert_eq!(err.code, 1054);
    assert_eq!(err.message, "Unknown column '9' in 'order clause'");
    // Before the grouping check, which is where 8.3 settles it: the same
    // statement with a name the clause cannot resolve is 1054 and not the 1055
    // that `ORDER BY b` under a different select list would raise.
    let err = refusal(
        &engine,
        &mut session,
        "SELECT MAX(id) AS b FROM O GROUP BY b ORDER BY nope",
    );
    assert_eq!(err.code, 1054);
    assert_eq!(err.message, "Unknown column 'nope' in 'order clause'");

    // A shadowed alias that falls through to an *ungrouped* column is 1055 on
    // 8.3, and this path has to say so rather than read the column off the
    // group's first row — an arbitrary member of it, and nondeterministic the
    // moment a group holds more than one. The cases above all group by `b`,
    // where the column is grouped and the question never arises; this one
    // groups by `s`, so `O.b` is neither grouped nor aggregated.
    let err = refusal(
        &engine,
        &mut session,
        "SELECT s AS b, COUNT(*) FROM O GROUP BY s ORDER BY b + 0",
    );
    assert_eq!(err.code, 1055, "expected the grouping complaint");
    // The alias reaches it under a name of its own too, so the refusal is
    // about the column the clause reads and not about the spelling.
    let err = refusal(
        &engine,
        &mut session,
        "SELECT s AS zz, COUNT(*) FROM O GROUP BY s ORDER BY b + 0",
    );
    assert_eq!(err.code, 1055);
    // After the select list, which 8.3 reports first, and after every name.
    let err = refusal(
        &engine,
        &mut session,
        "SELECT b, COUNT(*) FROM O GROUP BY s ORDER BY b",
    );
    assert_eq!(err.code, 1055);
    let err = refusal(
        &engine,
        &mut session,
        "SELECT s AS b, COUNT(*) FROM O GROUP BY s ORDER BY b + 0, nope",
    );
    assert_eq!(err.code, 1054);

    // What still answers: the bare alias, an aggregate over the ungrouped
    // column, and a grouped column the select list does not return.
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT s AS b, COUNT(*) FROM O GROUP BY s ORDER BY b"
        ),
        ["w", "x", "y", "z"]
    );
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT s, COUNT(*) FROM O GROUP BY s ORDER BY MAX(b)"
        ),
        ["w", "z", "y", "x"]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT COUNT(*) FROM O GROUP BY s ORDER BY s"
        )
        .len(),
        4
    );
}

/// A name two select items answer to is 1052, and one that only looks like it
/// is not.
///
/// Measured on 8.3: two items that are *different columns* are ambiguous —
/// `SELECT id AS b, b FROM O ORDER BY b` and `SELECT id AS x, b AS x FROM O
/// ORDER BY x` — and two that are the same column are not. An expression
/// sharing the name does not make it ambiguous either; the column wins,
/// wherever in the list it was written.
///
/// Without this the fix would have picked one of them and answered, which is
/// the wrong direction for a question MySQL refuses.
#[test]
fn an_ambiguous_order_by_name_is_refused() {
    let (engine, mut session) = engine();
    order_rows(&engine, &mut session);

    for (sql, name) in [
        ("SELECT id AS b, b FROM O ORDER BY b", "b"),
        ("SELECT id AS x, b AS x FROM O ORDER BY x", "x"),
        ("SELECT id AS x, b AS x FROM O ORDER BY x + 0", "x"),
        ("SELECT b AS id, id FROM O ORDER BY id", "id"),
    ] {
        let err = refusal(&engine, &mut session, sql);
        assert_eq!(err.code, 1052, "expected ER_NON_UNIQ_ERROR for: {sql}");
        assert_eq!(
            err.message,
            format!("Column '{name}' in order clause is ambiguous"),
            "for: {sql}"
        );
    }

    let ids = |engine: &Engine, session: &mut Session, sql: &str| -> Vec<String> {
        rows(engine, session, sql)
            .into_iter()
            .map(|row| row[0].clone())
            .collect()
    };
    // The same column twice, however it is spelled.
    assert_eq!(
        ids(&engine, &mut session, "SELECT b, O.b FROM O ORDER BY b"),
        ["1", "2", "3", "4"]
    );
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT id, id AS id FROM O ORDER BY id"
        ),
        ["1", "2", "3", "4"]
    );
    // A column and an expression sharing a name: the expression wins, from
    // either position in the list, and nothing after it is looked at. Written
    // DESC because `LENGTH(s)` is the same for every row here — sorting on it
    // leaves the rows as they arrived, which is the order sorting on `id`
    // ascending would also produce, and the two readings only separate once
    // the direction is reversed.
    assert_eq!(
        ids(
            &engine,
            &mut session,
            "SELECT id AS b, LENGTH(s) AS b FROM O ORDER BY b DESC"
        ),
        ["1", "2", "3", "4"]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT LENGTH(s) AS b, id AS b FROM O ORDER BY b DESC"
        ),
        vec![
            vec!["1", "1"],
            vec!["1", "2"],
            vec!["1", "3"],
            vec!["1", "4"]
        ]
    );
    // An ambiguity reached before that expression is still an ambiguity, and
    // one reached after it never happens.
    let err = refusal(
        &engine,
        &mut session,
        "SELECT id AS b, s AS b, 1 AS b FROM O ORDER BY b",
    );
    assert_eq!(err.code, 1052);
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT 1 AS b, id AS b, s AS b FROM O ORDER BY b"
        )
        .len(),
        4
    );
}

/// DISTINCT refuses an ORDER BY whose key it has already thrown away.
///
/// 8.3 raises 3065 rather than picking one of the duplicates. This engine
/// sorted the rows before removing them, which answered a question MySQL
/// declines to answer at all.
#[test]
fn distinct_refuses_an_order_by_the_select_list_does_not_carry() {
    let (engine, mut session) = engine();
    order_rows(&engine, &mut session);

    for (sql, column) in [
        ("SELECT DISTINCT s FROM O ORDER BY id", "id"),
        ("SELECT DISTINCT id FROM O ORDER BY b", "b"),
        ("SELECT DISTINCT id FROM O ORDER BY id + b", "b"),
        ("SELECT DISTINCT id + 1 AS x FROM O ORDER BY id", "id"),
    ] {
        let err = refusal(&engine, &mut session, sql);
        assert_eq!(
            err.code, 3065,
            "expected ER_FIELD_IN_ORDER_NOT_SELECT: {sql}"
        );
        assert_eq!(
            err.message,
            format!(
                "Expression #1 of ORDER BY clause is not in SELECT list, references column \
                 'testdb.O.{column}' which is not in SELECT list; this is incompatible with \
                 DISTINCT"
            ),
            "for: {sql}"
        );
    }

    // A derived table is in no schema, and 8.3 names its column `d.id` with
    // no leading dot. The engine used to write the empty schema anyway and
    // disagreed with itself, because a CTE's relation carries one.
    let err = refusal(
        &engine,
        &mut session,
        "SELECT DISTINCT s FROM (SELECT * FROM O) d ORDER BY id",
    );
    assert_eq!(err.code, 3065);
    assert_eq!(
        err.message,
        format!(
            "Expression #1 of ORDER BY clause is not in SELECT list, references column \
             '{}' which is not in SELECT list; this is incompatible with DISTINCT",
            "d.id"
        )
    );

    // Every item's names are resolved before any of them is asked about
    // DISTINCT: 8.3 answers this one for `nope` rather than for `id`.
    let err = refusal(
        &engine,
        &mut session,
        "SELECT DISTINCT s FROM O ORDER BY id, nope",
    );
    assert_eq!(err.code, 1054);
    assert_eq!(err.message, "Unknown column 'nope' in 'order clause'");

    // What the clause may still name under DISTINCT: a position, an alias, a
    // column the select list returns — including inside an expression — and
    // the select item written out a second time.
    for sql in [
        "SELECT DISTINCT s FROM O ORDER BY 1 DESC",
        "SELECT DISTINCT id AS b FROM O ORDER BY b",
        "SELECT DISTINCT id FROM O ORDER BY id + 1",
        "SELECT DISTINCT s FROM O ORDER BY LENGTH(s)",
        "SELECT DISTINCT id, b FROM O ORDER BY b",
        "SELECT DISTINCT id + 1 AS x FROM O ORDER BY id + 1",
        // Written a second time and spelled differently, which 8.3 accepts
        // because it compares the expressions rather than the source text.
        // Reading the text is what the first cut did, and it made all three
        // of these 3065.
        "SELECT DISTINCT id + 1 AS x FROM O ORDER BY id+1",
        "SELECT DISTINCT UPPER(s) FROM O ORDER BY upper(s)",
        "SELECT DISTINCT UPPER(s) AS u FROM O ORDER BY UPPER( s )",
    ] {
        assert_eq!(rows(&engine, &mut session, sql).len(), 4, "for: {sql}");
    }
}

/// An aggregate in the ORDER BY of a query that aggregates nothing is 3029.
///
/// The clause has no group to fold over, and 8.3 says so rather than treating
/// the aggregate as one covering the result. One inside a *subquery* belongs to
/// that subquery and is not this.
#[test]
fn an_aggregate_in_an_ungrouped_order_by_is_refused() {
    let (engine, mut session) = engine();
    order_rows(&engine, &mut session);

    let err = refusal(&engine, &mut session, "SELECT s FROM O ORDER BY MAX(id)");
    assert_eq!(err.code, 3029);
    assert_eq!(
        err.message,
        "Expression #1 of ORDER BY contains aggregate function and applies to the result of a \
         non-aggregated query"
    );
    // Its own item's names first, and the next item's not at all.
    let err = refusal(&engine, &mut session, "SELECT s FROM O ORDER BY MAX(nope)");
    assert_eq!(err.code, 1054);
    assert_eq!(err.message, "Unknown column 'nope' in 'order clause'");
    let err = refusal(&engine, &mut session, "SELECT s FROM O ORDER BY MAX(id), 9");
    assert_eq!(err.code, 3029);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT s FROM O ORDER BY (SELECT MAX(id) FROM O)"
        )
        .len(),
        4
    );
}

/// `GROUP BY <position>` counts over the select list too, and a position that
/// names nothing is 1054 under that clause's own name.
///
/// An unresolvable position used to be left as the literal it was written as,
/// which gives every row the same group key and collapses the table into one
/// row — an answer to a different query.
#[test]
fn a_group_by_position_that_names_nothing_is_refused() {
    let (engine, mut session) = engine();
    order_rows(&engine, &mut session);

    for (sql, name) in [
        ("SELECT id FROM O GROUP BY 5", "5"),
        ("SELECT id FROM O GROUP BY 0", "0"),
        ("SELECT id FROM O GROUP BY 5 ORDER BY 9", "5"),
    ] {
        let err = refusal(&engine, &mut session, sql);
        assert_eq!(err.code, 1054, "expected ER_BAD_FIELD_ERROR for: {sql}");
        assert_eq!(
            err.message,
            format!("Unknown column '{name}' in 'group statement'"),
            "for: {sql}"
        );
    }

    // After the field list, which 8.3 reports first.
    let err = refusal(&engine, &mut session, "SELECT nope FROM O GROUP BY 5");
    assert_eq!(err.code, 1054);
    assert_eq!(err.message, "Unknown column 'nope' in 'field list'");

    // A select item that *is* a number is not a position that failed to
    // resolve, which is the one thing checking the resolved clause would have
    // got wrong.
    assert_eq!(
        rows(&engine, &mut session, "SELECT 5 FROM O GROUP BY 1"),
        vec![vec!["5"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id FROM O GROUP BY 1 ORDER BY 1"
        )
        .len(),
        4
    );
}

/// A UNION's ORDER BY names the columns the union returns and nothing else.
///
/// `SELECT id AS q FROM O UNION SELECT b FROM O ORDER BY id` is 1054 on 8.3
/// even with `id` in plain sight in `O`, because what the clause sorts is the
/// combined result. The same holds for a SELECT with no FROM clause, where one
/// row sorts the same whatever the clause says but what it may name does not
/// widen because of it.
#[test]
fn an_order_by_over_a_finished_result_names_only_its_columns() {
    let (engine, mut session) = engine();
    order_rows(&engine, &mut session);

    for (sql, name) in [
        (
            "SELECT id AS q FROM O UNION SELECT b FROM O ORDER BY id",
            "id",
        ),
        ("SELECT 1 AS one ORDER BY nope", "nope"),
    ] {
        let err = refusal(&engine, &mut session, sql);
        assert_eq!(err.code, 1054, "expected ER_BAD_FIELD_ERROR for: {sql}");
        assert_eq!(
            err.message,
            format!("Unknown column '{name}' in 'order clause'"),
            "for: {sql}"
        );
    }

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id AS q FROM O UNION SELECT b FROM O ORDER BY q"
        ),
        vec![vec!["1"], vec!["2"], vec!["3"], vec!["4"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT 1 AS one ORDER BY one"),
        vec![vec!["1"]]
    );
    // The enclosing query's columns are in scope as well, so a correlated
    // subquery is not refused for naming one — 8.3 answers this.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, (SELECT 1 ORDER BY id) AS q FROM O ORDER BY id"
        ),
        vec![
            vec!["1", "1"],
            vec!["2", "1"],
            vec!["3", "1"],
            vec!["4", "1"]
        ]
    );
    let err = refusal(
        &engine,
        &mut session,
        "SELECT id, (SELECT 1 ORDER BY nope) AS q FROM O ORDER BY id",
    );
    assert_eq!(err.code, 1054);
    assert_eq!(err.message, "Unknown column 'nope' in 'order clause'");
}

/// A window function is still numbered down the order the rows are sorted in.
///
/// The guard on the fix's direction. Resolving the clause against the select
/// list must not become *projecting* first: 8.3 answers `SELECT id,
/// ROW_NUMBER() OVER () rn FROM O ORDER BY id DESC` with `rn` running 1 to 4
/// down the sorted rows, and computing the window before the sort would have
/// numbered them 4 to 1 instead. The rows are identical either way and only
/// the window value separates them, which is how #0103's version of this got
/// through review the first time.
///
/// The other half is that a position naming a window column still works: it
/// cannot be evaluated against a source row, so the sort is retried against
/// the projected result — where the window has been computed over the rows in
/// the order they arrived, and `ORDER BY 2 DESC` pairs `rn` 4 with `id` 4.
#[test]
fn a_window_is_numbered_over_the_rows_in_sorted_order() {
    let (engine, mut session) = engine();
    order_rows(&engine, &mut session);

    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, ROW_NUMBER() OVER () AS rn FROM O ORDER BY id DESC"
        ),
        vec![
            vec!["4", "1"],
            vec!["3", "2"],
            vec!["2", "3"],
            vec!["1", "4"]
        ]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, ROW_NUMBER() OVER () AS rn FROM O ORDER BY 2 DESC"
        ),
        vec![
            vec!["4", "4"],
            vec!["3", "3"],
            vec!["2", "2"],
            vec!["1", "1"]
        ]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT ROW_NUMBER() OVER (ORDER BY b) AS rn FROM O ORDER BY rn DESC"
        ),
        vec![vec!["4"], vec!["3"], vec!["2"], vec!["1"]]
    );
}

/// The `J2`/`K` schema #0098 was measured against, in `testdb` and `otherdb`.
///
/// `E` is empty on purpose: a refusal that only happens where a row is
/// evaluated is a refusal that does not happen at all over an empty relation,
/// which is the shape three separate issues under #0009 have now been.
fn two_databases(engine: &Engine, session: &mut Session) {
    run(engine, session, "CREATE DATABASE otherdb");
    run(engine, session, "CREATE TABLE J2 (a INT NULL, b INT NULL)");
    run(engine, session, "INSERT INTO J2 VALUES (1, 2), (3, 4)");
    run(engine, session, "CREATE TABLE K (a INT NULL, c INT NULL)");
    run(engine, session, "INSERT INTO K VALUES (1, 9)");
    run(engine, session, "CREATE TABLE E (a INT NULL, b INT NULL)");
    run(engine, session, "CREATE VIEW V AS SELECT b FROM J2");
    run(
        engine,
        session,
        "CREATE TABLE otherdb.T (a INT NULL, z INT NULL)",
    );
    run(engine, session, "INSERT INTO otherdb.T VALUES (1, 7)");
}

/// A three-part name is resolved against the database beside the table.
///
/// `column_slot` and `Relation::index_of` read `parts[len - 2]` as the table
/// and looked at nothing before it, so the database was compared against
/// nothing: `SELECT nodb.J2.b FROM J2` answered rows and `SELECT nodb.J2.*`
/// answered every column, where 8.3 is 1054 and 1051. The case that matters is
/// the one in the middle — a statement written against another schema silently
/// read this one.
///
/// Every refusal below is over a relation nothing is evaluated against: an
/// empty table, `WHERE 1=0`, `LIMIT 0`, a CASE branch nothing takes. The check
/// has to be where the item is resolved, because that is where 8.3 settles it
/// — see #0058, #0093 and #0090, which were the same mistake in three other
/// clauses.
#[test]
fn a_database_qualifier_must_name_the_database_the_table_is_in() {
    let (engine, mut session) = engine();
    two_databases(&engine, &mut session);

    for sql in [
        // The two statements the issue was written from.
        "SELECT nodb.J2.b FROM J2",
        "SELECT otherdb.J2.b FROM J2",
        // ... and the same reference where no row can settle it. Each of
        // these answered a successful empty result set.
        "SELECT nodb.E.b FROM E",
        "SELECT nodb.J2.b FROM J2 WHERE 1 = 0",
        "SELECT nodb.J2.b FROM J2 LIMIT 0",
        "SELECT CASE WHEN 0 THEN nodb.J2.b ELSE 9 END FROM J2",
        // Every other clause that resolves a name, each answered on 8.3 under
        // its own clause name and all 1054 there.
        "SELECT b FROM J2 WHERE nodb.J2.a = 1",
        "SELECT b FROM J2 GROUP BY nodb.J2.b",
        "SELECT b FROM J2 GROUP BY b HAVING nodb.J2.b > 0",
        "SELECT b FROM J2 ORDER BY nodb.J2.b",
        "SELECT J2.b FROM J2 JOIN K ON nodb.J2.a = K.a",
        "SELECT b FROM J2 WHERE a IN (SELECT a FROM K WHERE nodb.J2.b = 2)",
        "SELECT nodb.J2.b FROM J2 UNION SELECT 1",
        // An alias renames the table part and leaves the database alone, so a
        // wrong database is still wrong through one.
        "SELECT nodb.x.b FROM J2 x",
        // The cross-database direction: `T` is in `otherdb`, and naming this
        // database for it is 1054 on 8.3 exactly as naming one that does not
        // exist is.
        "SELECT testdb.T.z FROM otherdb.T",
        "SELECT testdb.x.z FROM otherdb.T x",
        // A view is in a database, unlike the query behind it.
        "SELECT nodb.V.b FROM V",
        // More parts than MySQL's grammar has levels for. 8.3 answers 1064
        // here and this engine answers 1054; what matters is that it stops
        // reading the last two parts and answering rows.
        "SELECT w.testdb.J2.b FROM J2",
    ] {
        let err = refusal_of(&engine, &mut session, sql);
        assert_eq!(err.code, 1054, "expected ER_BAD_FIELD_ERROR for: {sql}");
    }

    // The wildcard's code is 1051, not 1054, and it names the qualifier as it
    // was written rather than the column.
    for sql in [
        "SELECT nodb.J2.* FROM J2",
        "SELECT otherdb.J2.* FROM J2",
        "SELECT nodb.E.* FROM E",
        "SELECT nodb.x.* FROM J2 x",
        "SELECT nodb.V.* FROM V",
        "SELECT testdb.T.* FROM otherdb.T",
    ] {
        let err = refusal_of(&engine, &mut session, sql);
        assert_eq!(err.code, 1051, "expected ER_BAD_TABLE_ERROR for: {sql}");
    }
    assert_eq!(
        refusal_of(&engine, &mut session, "SELECT nodb.J2.* FROM J2").message,
        "Unknown table 'nodb.J2'"
    );
    assert_eq!(
        refusal_of(&engine, &mut session, "SELECT nodb.J2.b FROM J2").message,
        "Unknown column 'nodb.J2.b' in 'field list'"
    );

    // The right database still answers, which is the half a fix can break by
    // refusing everything: the current database spelled explicitly, the table
    // qualified by it in the FROM clause, an alias, and a view.
    for sql in [
        "SELECT testdb.J2.b FROM J2 ORDER BY 1",
        "SELECT testdb.J2.b FROM testdb.J2 ORDER BY 1",
        "SELECT J2.b FROM testdb.J2 ORDER BY 1",
        "SELECT testdb.x.b FROM J2 x ORDER BY 1",
        "SELECT testdb.V.b FROM V ORDER BY 1",
        "SELECT b FROM J2 WHERE testdb.J2.a = 1",
        "SELECT b FROM J2 GROUP BY testdb.J2.b ORDER BY 1",
        "SELECT b FROM J2 GROUP BY b HAVING testdb.J2.b > 0 ORDER BY 1",
        "SELECT b FROM J2 ORDER BY testdb.J2.b",
        "SELECT J2.b FROM J2 JOIN K ON testdb.J2.a = K.a",
        "SELECT b FROM J2 WHERE a IN (SELECT a FROM K WHERE testdb.J2.b = 2)",
    ] {
        run(&engine, &mut session, sql);
    }
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT testdb.J2.* FROM J2 ORDER BY 1"
        ),
        vec![vec!["1", "2"], vec!["3", "4"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT testdb.x.* FROM J2 x ORDER BY 1"
        ),
        vec![vec!["1", "2"], vec!["3", "4"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT otherdb.T.z FROM otherdb.T"),
        vec![vec!["7"]]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT otherdb.T.* FROM otherdb.T"),
        vec![vec!["1", "7"]]
    );

    // information_schema is a database like any other here, and answers to its
    // own name and to nothing else.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT information_schema.tables.TABLE_NAME FROM information_schema.tables \
             WHERE TABLE_SCHEMA = 'testdb' AND TABLE_NAME = 'K'"
        ),
        vec![vec!["K"]]
    );
    assert_eq!(
        refusal_of(
            &engine,
            &mut session,
            "SELECT testdb.tables.TABLE_NAME FROM information_schema.tables"
        )
        .code,
        1054
    );
}

/// A FROM item that is in no database ignores the qualifier on a column and
/// refuses it on a wildcard.
///
/// Both halves measured on 8.3, over the same derived table: `SELECT nodb.d.b`
/// answers and `SELECT testdb.d.*` — the database spelled *correctly* — is
/// `Unknown table 'testdb.d'`. A derived table, a CTE and a JSON_TABLE have no
/// database for the qualifier to be compared against, and the two clauses read
/// that absence in opposite directions. A fix that compared the database one
/// way everywhere gets one of these wrong.
#[test]
fn a_from_item_in_no_database_answers_every_column_qualifier_and_no_wildcard() {
    let (engine, mut session) = engine();
    two_databases(&engine, &mut session);

    for sql in [
        "SELECT nodb.d.b FROM (SELECT b FROM J2) d ORDER BY 1",
        "SELECT testdb.d.b FROM (SELECT b FROM J2) d ORDER BY 1",
        "WITH c AS (SELECT b FROM J2) SELECT nodb.c.b FROM c ORDER BY 1",
        "WITH c AS (SELECT b FROM J2) SELECT testdb.c.b FROM c ORDER BY 1",
    ] {
        assert_eq!(
            rows(&engine, &mut session, sql),
            vec![vec!["2"], vec!["4"]],
            "expected an answer for: {sql}"
        );
    }
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT nodb.jt.v FROM JSON_TABLE('[1,2]', '$[*]' COLUMNS(v INT PATH '$')) jt \
             ORDER BY 1"
        ),
        vec![vec!["1"], vec!["2"]]
    );

    for sql in [
        "SELECT testdb.d.* FROM (SELECT b FROM J2) d",
        "SELECT nodb.d.* FROM (SELECT b FROM J2) d",
        "WITH c AS (SELECT b FROM J2) SELECT testdb.c.* FROM c",
        "SELECT testdb.jt.* FROM JSON_TABLE('[1,2]', '$[*]' COLUMNS(v INT PATH '$')) jt",
    ] {
        let err = refusal_of(&engine, &mut session, sql);
        assert_eq!(err.code, 1051, "expected ER_BAD_TABLE_ERROR for: {sql}");
    }

    // The unqualified wildcard over the same item still answers, so the
    // refusal above is about the database and not about the item.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT d.* FROM (SELECT b FROM J2) d ORDER BY 1"
        ),
        vec![vec!["2"], vec!["4"]]
    );
}

/// A write whose target is qualified by the wrong database is refused.
///
/// The worst shape of this bug: `UPDATE J2 x JOIN K ... SET nodb.x.b = 1`
/// wrote to `testdb`.`J2`.`b`, and `INSERT INTO J2 (nodb.J2.a)` inserted into
/// `testdb`.`J2`.`a`, because both resolved the target by its last part alone.
/// A wrong answer a client cannot see, since the statement reports rows
/// affected exactly as a correct one would.
///
/// The single-table `UPDATE`'s and `DELETE`'s own names are #0102's: they
/// resolve nothing at prepare time, so there is no qualifier there to compare
/// yet. What they do resolve — per row, while evaluating — now carries the
/// database, which is the last block below.
#[test]
fn a_write_qualified_by_the_wrong_database_is_refused() {
    let (engine, mut session) = engine();
    two_databases(&engine, &mut session);

    // The message names every part, not just the column: which qualifier was
    // wrong is the whole content of the refusal.
    assert_eq!(
        refusal_of(
            &engine,
            &mut session,
            "UPDATE J2 x JOIN K ON x.a = K.a SET nodb.x.b = 1"
        )
        .message,
        "Unknown column 'nodb.x.b' in 'field list'"
    );
    assert_eq!(
        refusal_of(&engine, &mut session, "UPDATE J2 SET nodb.J2.b = 1").message,
        "Unknown column 'nodb.J2.b' in 'field list'"
    );
    assert_eq!(
        refusal_of(
            &engine,
            &mut session,
            "INSERT INTO J2 (nodb.J2.a) VALUES (9)"
        )
        .message,
        "Unknown column 'nodb.J2.a' in 'field list'"
    );

    for sql in [
        "UPDATE J2 x JOIN K ON x.a = K.a SET nodb.x.b = 1",
        "UPDATE J2 x JOIN K ON x.a = K.a SET otherdb.x.b = 1",
        "INSERT INTO J2 (nodb.J2.a) VALUES (9)",
        "INSERT INTO J2 (otherdb.J2.a) VALUES (9)",
        // The table half of the same qualifier, which was discarded too:
        // `K` is in scope and is not the table being inserted into.
        "INSERT INTO J2 (K.a) VALUES (9)",
        "INSERT INTO J2 (a, b) VALUES (1, 2) ON DUPLICATE KEY UPDATE nodb.J2.b = 1",
        // The single-table SET target, which read its last part and nothing
        // else — so the table half of the qualifier was discarded too.
        "UPDATE J2 SET nodb.J2.b = 1",
        "UPDATE J2 SET otherdb.J2.b = 1",
        "UPDATE J2 SET K.b = 1",
        "UPDATE J2 x SET nodb.x.b = 1",
        // Over an empty table as well: the targets are resolved before any row
        // is considered, so this does not depend on there being one.
        "UPDATE E SET nodb.E.b = 1",
    ] {
        let err = refusal_of(&engine, &mut session, sql);
        assert_eq!(err.code, 1054, "expected ER_BAD_FIELD_ERROR for: {sql}");
    }

    // The right database writes, as on 8.3.
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "UPDATE J2 x JOIN K ON x.a = K.a SET testdb.x.b = 5"
        ),
        1
    );
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "INSERT INTO J2 (testdb.J2.a) VALUES (9)"
        ),
        1
    );
    assert_eq!(
        affected(&engine, &mut session, "INSERT INTO J2 (J2.a) VALUES (10)"),
        1
    );
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "UPDATE J2 SET testdb.J2.b = 7 WHERE a = 10"
        ),
        1
    );
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "UPDATE J2 x SET testdb.x.b = 8 WHERE a = 10"
        ),
        1
    );
    assert_eq!(
        affected(&engine, &mut session, "UPDATE E SET testdb.E.b = 1"),
        0
    );

    // Read per row rather than resolved up front — #0102 — so this bites only
    // where a row reaches the evaluation. Over an empty table it still
    // answers, which is that issue and not this one.
    for sql in [
        "UPDATE J2 SET b = b WHERE nodb.J2.a = 1",
        "UPDATE J2 SET b = nodb.J2.b",
        "DELETE FROM J2 WHERE nodb.J2.a = 1",
    ] {
        let err = refusal_of(&engine, &mut session, sql);
        assert_eq!(err.code, 1054, "expected ER_BAD_FIELD_ERROR for: {sql}");
    }
    assert_eq!(
        affected(
            &engine,
            &mut session,
            "DELETE FROM J2 WHERE testdb.J2.a = 10"
        ),
        1
    );
}

/// A sort key the source rows answered is kept when another one cannot be.
///
/// `sort_rows` used to be all-or-nothing in both directions: one key it could
/// not evaluate against a source row abandoned the sort entirely, and the
/// retry then re-derived *every* key from the projected output — where a key
/// naming a column the select list does not return no longer exists. So each
/// key worked alone and the mixture was 1054, under the field list's name at
/// that:
///
/// ```text
/// SELECT id, ROW_NUMBER() OVER () rn FROM t ORDER BY 2, b   -- 1054 'b'
/// SELECT id, ROW_NUMBER() OVER () rn FROM t ORDER BY 2      -- answered
/// SELECT id, ROW_NUMBER() OVER () rn FROM t ORDER BY b      -- answered
/// ```
///
/// The order is the assertion, not just the absence of an error. 8.3 numbers a
/// window down the order the rows *reach* it and sorts afterwards, so the
/// half-sort a partial fix would apply — ordering the source rows by the key
/// that did resolve — renumbers the window and answers differently. Every
/// expectation below was read off 8.3.
#[test]
fn a_window_sort_key_does_not_discard_the_keys_beside_it() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Wk (id INT NULL, b INT NULL, s VARCHAR(10) NULL)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Wk VALUES (1, 20, 'x'), (2, 10, 'y'), (3, 30, 'x')",
    );

    // A position naming the window, then a column the select list omits. `rn`
    // is 1,2,3 down the table's order — not down `b`'s — and the rows come
    // back in `rn` order.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, ROW_NUMBER() OVER () rn FROM Wk ORDER BY 2, b"
        ),
        vec![vec!["1", "1"], vec!["2", "2"], vec!["3", "3"]]
    );
    // The omitted column first and the window second, which is the ordering a
    // two-pass fix cannot get right: the least significant key is the one that
    // only exists after the projection.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT ROW_NUMBER() OVER () rn, s FROM Wk ORDER BY b, 1"
        ),
        vec![vec!["2", "y"], vec!["1", "x"], vec!["3", "x"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, ROW_NUMBER() OVER () rn FROM Wk ORDER BY b, 2"
        ),
        vec![vec!["2", "2"], vec!["1", "1"], vec!["3", "3"]]
    );
    // Named rather than positioned, and descending, so the flags travel with
    // the carried keys rather than being re-derived beside them.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, ROW_NUMBER() OVER () rn FROM Wk ORDER BY rn DESC, b"
        ),
        vec![vec!["3", "3"], vec!["2", "2"], vec!["1", "1"]]
    );
    // An alias inside a larger expression reaches the same retry, without a
    // window anywhere: `q` is substituted for its expression, which the source
    // row cannot answer either.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, id * 10 q FROM Wk ORDER BY CASE WHEN q > 1 THEN 0 ELSE 1 END, b"
        ),
        vec![vec!["2", "20"], vec!["1", "10"], vec!["3", "30"]]
    );
    // DISTINCT collapses rows after the sort, which is why the sort moved
    // ahead of it: the carried keys line up with the projected rows one for
    // one and a dedup between the two would break that.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT DISTINCT s, ROW_NUMBER() OVER () rn FROM Wk ORDER BY rn DESC"
        ),
        vec![vec!["x", "3"], vec!["y", "2"], vec!["x", "1"]]
    );
    // And the LIMIT is applied to the finished order rather than to the
    // half-sorted one.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, ROW_NUMBER() OVER () rn FROM Wk ORDER BY 2, b LIMIT 2"
        ),
        vec![vec!["1", "1"], vec!["2", "2"]]
    );
    // Each key alone, which worked before and has to keep working.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, ROW_NUMBER() OVER () rn FROM Wk ORDER BY 2"
        ),
        vec![vec!["1", "1"], vec!["2", "2"], vec!["3", "3"]]
    );
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT id, ROW_NUMBER() OVER () rn FROM Wk ORDER BY b"
        ),
        vec![vec!["2", "1"], vec!["1", "2"], vec!["3", "3"]]
    );
}

/// Two qualified wildcards over a `USING` join put the joined column in the
/// output twice, and a bare `ORDER BY` on its name is then ambiguous.
///
/// Measured on 8.3 rather than reasoned about, because it has been argued both
/// ways: `SELECT t.*, u.* FROM t JOIN u USING (id) ORDER BY id` is
/// `Column 'id' in order clause is ambiguous` there, in either order of the
/// two wildcards, and `GROUP BY id` is the same error under its own clause
/// name. Only the bare `*` coalesces — `SELECT * FROM t JOIN u USING (id)`
/// returns one `id`.
///
/// The distinction is which scope the clause reads. `WHERE id = 1` resolves
/// against the FROM clause, where `USING` really has made the two columns one,
/// and answers; ORDER BY resolves against the *output*, which has two.
#[test]
fn a_qualified_wildcard_does_not_coalesce_a_using_column() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Ut (id INT NULL, b INT NULL)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Ut VALUES (1, 20), (2, 10)",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE Uu (id INT NULL, z INT NULL)",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO Uu VALUES (1, 7), (2, 8)",
    );

    for sql in [
        "SELECT Ut.*, Uu.* FROM Ut JOIN Uu USING (id) ORDER BY id",
        "SELECT Uu.*, Ut.* FROM Ut JOIN Uu USING (id) ORDER BY id",
    ] {
        let err = refusal_of(&engine, &mut session, sql);
        assert_eq!(err.code, 1052, "expected ER_NON_UNIQ_ERROR for: {sql}");
        assert_eq!(err.message, "Column 'id' in order clause is ambiguous");
    }

    // A position names one output column outright, so it is never ambiguous.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT Ut.*, Uu.* FROM Ut JOIN Uu USING (id) ORDER BY 1"
        ),
        vec![vec!["1", "20", "1", "7"], vec!["2", "10", "2", "8"]]
    );
    // One wildcard leaves one `id` in the output, and the same clause answers.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT Ut.* FROM Ut JOIN Uu USING (id) ORDER BY id"
        ),
        vec![vec!["1", "20"], vec!["2", "10"]]
    );
    // The WHERE reads the FROM clause, where USING has made them one column.
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT Ut.*, Uu.* FROM Ut JOIN Uu USING (id) WHERE id = 1"
        ),
        vec![vec!["1", "20", "1", "7"]]
    );
}
