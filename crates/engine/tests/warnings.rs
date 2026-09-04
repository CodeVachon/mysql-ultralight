//! The diagnostics area: what a statement raises, and how long it lives.
//!
//! Every triple asserted here was measured on mysql:8.3 through
//! `scripts/ask.sh` before it was implemented, because the codes are not
//! guessable from the code names: 1466 is ER_REMOVED_SPACES and its message
//! says `Leading spaces are removed from name` for an alias that was cut for
//! being too long, and 1265 arrives at two different *levels* depending on
//! whether anything the client wrote was lost. #0204 and #0168.

use mysql_ultralight_engine::{Engine, Outcome, ServerConfig, Session, Value};

fn engine() -> (std::sync::Arc<Engine>, Session) {
    let engine = Engine::new(ServerConfig::default());
    engine.bootstrap_database("testdb").unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    (engine, session)
}

fn run(engine: &Engine, session: &mut Session, sql: &str) -> Outcome {
    engine
        .execute(session, sql, &[])
        .unwrap_or_else(|e| panic!("{sql}\n  -> {e}"))
}

fn rows(engine: &Engine, session: &mut Session, sql: &str) -> Vec<Vec<String>> {
    match run(engine, session, sql) {
        Outcome::Rows(r) => r
            .rows
            .iter()
            .map(|row| row.iter().map(Value::as_text).collect())
            .collect(),
        Outcome::Affected { .. } => panic!("expected rows from: {sql}"),
    }
}

/// `SHOW WARNINGS` as it reaches a client, level first.
fn warnings(engine: &Engine, session: &mut Session) -> Vec<Vec<String>> {
    rows(engine, session, "SHOW WARNINGS")
}

/// The one number `SHOW COUNT(*) WARNINGS` answers with.
fn count(engine: &Engine, session: &mut Session) -> String {
    rows(engine, session, "SHOW COUNT(*) WARNINGS")
        .pop()
        .and_then(|mut row| row.pop())
        .expect("a single count row")
}

/// A table wide enough to make every kind of write fail, under a non-strict
/// mode so the failures truncate and clamp rather than being refused.
fn lax(engine: &Engine, session: &mut Session) {
    run(engine, session, "SET SESSION sql_mode = ''");
    run(
        engine,
        session,
        "CREATE TABLE T (a VARCHAR(3) NULL, b TINYINT NULL, c CHAR(3) NULL, g VARBINARY(3) NULL)",
    );
}

fn row(level: &str, code: u16, message: &str) -> Vec<String> {
    vec![level.to_owned(), code.to_string(), message.to_owned()]
}

/// An alias past the 256-byte cut, and long enough that the cut is obvious in
/// a failure message.
fn long_alias() -> String {
    "a".repeat(300)
}

#[test]
fn a_truncating_write_raises_1265_at_the_level_that_says_what_was_lost() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);

    // Characters the column will not hold: a Warning.
    run(&engine, &mut session, "INSERT INTO T (a) VALUES ('abcdef')");
    assert_eq!(
        warnings(&engine, &mut session),
        vec![row(
            "Warning",
            1265,
            "Data truncated for column 'a' at row 1"
        )]
    );

    // Only spaces past the width, so nothing written is lost: a Note. The
    // level is the whole difference, and it is the part no code name predicts.
    run(&engine, &mut session, "INSERT INTO T (a) VALUES ('abc  ')");
    assert_eq!(
        warnings(&engine, &mut session),
        vec![row("Note", 1265, "Data truncated for column 'a' at row 1")]
    );

    // A CHAR gives up its trailing spaces as a matter of what the type is, not
    // as a truncation: 8.3 raises nothing at all here.
    run(&engine, &mut session, "INSERT INTO T (c) VALUES ('ab   ')");
    assert!(warnings(&engine, &mut session).is_empty());

    // A binary column has no space exemption to be inside of.
    run(&engine, &mut session, "INSERT INTO T (g) VALUES ('abcdef')");
    assert_eq!(
        warnings(&engine, &mut session),
        vec![row(
            "Warning",
            1265,
            "Data truncated for column 'g' at row 1"
        )]
    );
}

#[test]
fn a_clamped_integer_raises_1264_at_both_ends() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);

    for value in ["999", "-999"] {
        run(
            &engine,
            &mut session,
            &format!("INSERT INTO T (b) VALUES ({value})"),
        );
        assert_eq!(
            warnings(&engine, &mut session),
            vec![row(
                "Warning",
                1264,
                "Out of range value for column 'b' at row 1"
            )],
            "clamping {value}"
        );
    }
}

/// One condition per offending row, numbered by position in the VALUES list —
/// so row 2, which fits, is skipped and row 3 is still called row 3.
#[test]
fn a_multi_row_write_numbers_the_row_it_is_talking_about() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);

    run(
        &engine,
        &mut session,
        "INSERT INTO T (a, b) VALUES ('abcdef', 1), ('ok', 2), ('zzzzzz', 999)",
    );
    assert_eq!(
        warnings(&engine, &mut session),
        vec![
            row("Warning", 1265, "Data truncated for column 'a' at row 1"),
            row("Warning", 1265, "Data truncated for column 'a' at row 3"),
            row(
                "Warning",
                1264,
                "Out of range value for column 'b' at row 3"
            ),
        ]
    );
    assert_eq!(count(&engine, &mut session), "3");
}

/// The rule that made the first probe written for this look like agreement: a
/// statement replaces the area, so a label `SELECT` between the write and the
/// `SHOW WARNINGS` empties it.
#[test]
fn a_statement_that_raises_nothing_clears_the_area() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);

    run(&engine, &mut session, "INSERT INTO T (a) VALUES ('abcdef')");
    assert_eq!(count(&engine, &mut session), "1");

    run(&engine, &mut session, "SELECT 1");
    assert!(warnings(&engine, &mut session).is_empty());
    assert_eq!(count(&engine, &mut session), "0");
}

/// And the exception: the readers leave it exactly as they found it, which is
/// what lets a client ask for the count and then the list.
#[test]
fn reading_the_area_does_not_empty_it() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);

    run(&engine, &mut session, "INSERT INTO T (a) VALUES ('abcdef')");
    let expected = vec![row(
        "Warning",
        1265,
        "Data truncated for column 'a' at row 1",
    )];

    assert_eq!(warnings(&engine, &mut session), expected);
    assert_eq!(count(&engine, &mut session), "1");
    assert_eq!(
        rows(&engine, &mut session, "SHOW WARNINGS LIMIT 1"),
        expected
    );
    assert_eq!(warnings(&engine, &mut session), expected, "still there");
}

/// `@@warning_count` is *not* one of the readers. It answers the count the
/// previous statement left and then clears like any other statement, so three
/// reads in a row are 1, 0, 0 — measured on 8.3.
#[test]
fn reading_the_count_as_a_variable_still_clears_the_area() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);

    run(&engine, &mut session, "INSERT INTO T (a) VALUES ('abcdef')");
    let reads: Vec<String> = (0..3)
        .map(|_| {
            rows(&engine, &mut session, "SELECT @@warning_count")
                .remove(0)
                .remove(0)
        })
        .collect();
    assert_eq!(reads, vec!["1", "0", "0"]);
    assert!(warnings(&engine, &mut session).is_empty());
}

/// A failed statement leaves its own error behind as a condition, after
/// whatever it raised before failing. Under a strict mode an INSERT whose
/// first row only trims spaces and whose second is too long leaves both.
#[test]
fn a_failed_statement_leaves_its_error_in_the_area() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "SET SESSION sql_mode = 'STRICT_ALL_TABLES'",
    );

    let refused = engine
        .execute(
            &mut session,
            "INSERT INTO T (a) VALUES ('abc  '), ('abcdef')",
            &[],
        )
        .expect_err("strict mode refuses the second row");
    assert_eq!(refused.code, 1406);

    assert_eq!(
        warnings(&engine, &mut session),
        vec![
            row("Note", 1265, "Data truncated for column 'a' at row 1"),
            row("Error", 1406, "Data too long for column 'a' at row 2"),
        ]
    );
    // Both counts, because they answer different questions: every condition,
    // and only the Error-level ones.
    assert_eq!(count(&engine, &mut session), "2");
    assert_eq!(
        rows(&engine, &mut session, "SHOW COUNT(*) ERRORS"),
        vec![vec!["1".to_owned()]]
    );
    // SHOW ERRORS filters to the Error level; SHOW WARNINGS shows every level.
    assert_eq!(
        rows(&engine, &mut session, "SHOW ERRORS"),
        vec![row("Error", 1406, "Data too long for column 'a' at row 2")]
    );
}

/// The trailing-space trim is a Note under *every* mode, which is why the test
/// above can assert one beside an error: strict turns the lossy truncation
/// into 1406 and leaves this one alone.
#[test]
fn strict_mode_still_notes_a_trailing_space_trim() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "SET SESSION sql_mode = 'STRICT_ALL_TABLES'",
    );

    run(&engine, &mut session, "INSERT INTO T (a) VALUES ('abc  ')");
    assert_eq!(
        warnings(&engine, &mut session),
        vec![row("Note", 1265, "Data truncated for column 'a' at row 1")]
    );
}

/// IGNORE is the other half of `types::lenient`: it truncates under a strict
/// mode, and 8.3 carries the same warning for it.
#[test]
fn insert_ignore_under_a_strict_mode_warns_rather_than_going_quiet() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "SET SESSION sql_mode = 'STRICT_ALL_TABLES'",
    );

    run(
        &engine,
        &mut session,
        "INSERT IGNORE INTO T (a) VALUES ('abcdef')",
    );
    assert_eq!(
        warnings(&engine, &mut session),
        vec![row(
            "Warning",
            1265,
            "Data truncated for column 'a' at row 1"
        )]
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT a FROM T"),
        vec![vec!["abc".to_owned()]],
        "what is stored is unchanged; only the report of it is new"
    );
}

#[test]
fn show_warnings_takes_a_limit_with_and_without_an_offset() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);
    run(
        &engine,
        &mut session,
        "INSERT INTO T (a) VALUES ('abcdef'), ('zzzzzz'), ('yyyyyy'), ('wwwwww')",
    );

    let at = |n: usize| {
        row(
            "Warning",
            1265,
            &format!("Data truncated for column 'a' at row {n}"),
        )
    };

    assert_eq!(
        rows(&engine, &mut session, "SHOW WARNINGS LIMIT 2"),
        vec![at(1), at(2)]
    );
    assert_eq!(
        rows(&engine, &mut session, "SHOW WARNINGS LIMIT 1, 2"),
        vec![at(2), at(3)]
    );
    assert!(rows(&engine, &mut session, "SHOW WARNINGS LIMIT 0").is_empty());
    assert_eq!(
        rows(&engine, &mut session, "SHOW WARNINGS LIMIT 99").len(),
        4,
        "a limit past the end is the whole list"
    );
}

/// `max_error_count` caps the list and not the count, which is the pair a
/// client reads as "two rows here, five happened".
#[test]
fn max_error_count_caps_the_list_and_leaves_the_count_alone() {
    let (engine, mut session) = engine();
    lax(&engine, &mut session);
    run(&engine, &mut session, "SET SESSION max_error_count = 2");

    run(
        &engine,
        &mut session,
        "INSERT INTO T (a) VALUES ('aaaaaa'), ('bbbbbb'), ('cccccc'), ('dddddd'), ('eeeeee')",
    );
    assert_eq!(warnings(&engine, &mut session).len(), 2);
    assert_eq!(count(&engine, &mut session), "5");

    // At zero, nothing is kept at all -- the statement's own error included.
    run(&engine, &mut session, "SET SESSION max_error_count = 0");
    assert!(engine
        .execute(&mut session, "SELECT * FROM NoSuchTable", &[])
        .is_err());
    assert!(warnings(&engine, &mut session).is_empty());
    assert_eq!(count(&engine, &mut session), "1");
}

#[test]
fn max_error_count_is_clamped_and_the_two_counters_are_read_only() {
    let (engine, mut session) = engine();

    for (set, expected) in [("-1", "0"), ("99999999", "65535"), ("1024", "1024")] {
        run(
            &engine,
            &mut session,
            &format!("SET SESSION max_error_count = {set}"),
        );
        assert_eq!(
            rows(&engine, &mut session, "SELECT @@max_error_count").remove(0),
            vec![expected.to_owned()],
            "setting {set}"
        );
    }
    assert_eq!(
        engine
            .execute(&mut session, "SET SESSION max_error_count = 'abc'", &[])
            .expect_err("a non-integer is refused")
            .code,
        1232
    );

    // Refused rather than stored, because an override would shadow the
    // diagnostics area these are read out of.
    for name in ["warning_count", "error_count"] {
        let refused = engine
            .execute(&mut session, &format!("SET SESSION {name} = 5"), &[])
            .expect_err("a read-only variable is refused");
        assert_eq!(refused.code, 1238);
        assert_eq!(
            refused.message,
            format!("Variable '{name}' is a read only variable")
        );
    }
}

/// #0168. The cut alias, and the two things about it that no reading of the
/// code would have produced: the message is ER_REMOVED_SPACES' wording, and it
/// quotes the alias **uncut** while the column is named with the first 256
/// bytes.
#[test]
fn a_cut_alias_raises_1466_naming_the_alias_as_written() {
    let (engine, mut session) = engine();
    let alias = long_alias();

    let Outcome::Rows(result) = run(&engine, &mut session, &format!("SELECT 1 AS `{alias}`"))
    else {
        panic!("expected rows");
    };
    assert_eq!(result.columns[0].name.len(), 256, "the name is still cut");

    assert_eq!(
        warnings(&engine, &mut session),
        vec![row(
            "Warning",
            1466,
            &format!("Leading spaces are removed from name '{alias}'")
        )]
    );
}

/// Once per alias however many readers ask for it, and once *each* for two
/// aliases spelled identically — which is why the condition is keyed on where
/// the alias is and not on what it says. 8.3 answers 2 for the second query.
#[test]
fn a_cut_alias_is_reported_once_per_alias_and_not_once_per_reader() {
    let (engine, mut session) = engine();
    let alias = long_alias();

    // An ORDER BY over the alias is a second reader of the same cut, and the
    // grouped path is a third.
    run(
        &engine,
        &mut session,
        &format!("SELECT 1 AS `{alias}` ORDER BY `{}`", &alias[..256]),
    );
    assert_eq!(count(&engine, &mut session), "1");

    run(
        &engine,
        &mut session,
        &format!("SELECT 1 AS `{alias}`, 2 AS `{alias}`"),
    );
    assert_eq!(count(&engine, &mut session), "2");
}

/// A short alias is not touched, so nothing here fires on ordinary SQL.
#[test]
fn an_alias_within_the_limit_raises_nothing() {
    let (engine, mut session) = engine();
    run(&engine, &mut session, "SELECT 1 AS `short`");
    assert!(warnings(&engine, &mut session).is_empty());
}

/// A spelling this cannot read is refused rather than answered as the nearest
/// thing it can read: `SHOW WARNINGS LIMIT x` must not come back as an
/// unlimited list.
#[test]
fn a_diagnostics_statement_this_cannot_read_is_an_error() {
    let (engine, mut session) = engine();
    for sql in [
        "SHOW WARNINGS LIMIT x",
        "SHOW WARNINGS LIMIT 1, 2, 3",
        "SHOW COUNT(*) NOTES",
    ] {
        assert!(
            engine.execute(&mut session, sql, &[]).is_err(),
            "{sql} must be refused"
        );
    }
}
