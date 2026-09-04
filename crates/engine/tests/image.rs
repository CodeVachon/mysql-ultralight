//! The state image: what it round trips, and how it fails.
//!
//! A corrupt or stale image must fail loudly. The alternative is a test process
//! that boots with a catalog missing tables, and a failure somewhere far from
//! the cause.

use mysql_ultralight_engine::{image, Engine, Outcome, ServerConfig, Session, Value};

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

/// Aliased so a test that needs a *second* engine can reach it after binding a
/// local named `engine`.
fn fresh_engine() -> (std::sync::Arc<Engine>, Session) {
    engine()
}

/// A directory under the system temp directory that belongs to one test.
///
/// These tests used fixed names (`mu-image-describe.mus`), which two runs of
/// the suite at once shared: one run's snapshot was the other run's file to
/// delete, and the test that reported it was whichever one next read a file
/// somebody else had removed. That failure names a snapshot or a describe
/// behaviour rather than the collision, and running the named test on its own
/// passes, so it costs a whole debugging session to place.
///
/// Built from std rather than a temp-dir crate on purpose: `cargo audit` and
/// `cargo machete` carry every dependency in this workspace forever, and a
/// directory name and a `remove_dir_all` do not need one.
struct Scratch {
    root: std::path::PathBuf,
}

impl Scratch {
    fn new() -> Self {
        // The process id separates concurrent runs and the counter separates
        // the tests inside one; the clock is what separates this run from a
        // directory left by a killed process whose id has since been handed
        // out again, which the other two cannot do.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root =
            std::env::temp_dir().join(format!("mu-image-{}-{now:x}-{ordinal}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.root.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A panicking test unwinds through this, so the failure path leaves no
        // more behind than the passing one. Files left in a shared directory
        // by a failing run are the same bug at a smaller size.
        std::fs::remove_dir_all(&self.root).ok();
    }
}

#[test]
fn two_scratch_directories_are_never_the_same_directory() {
    // The property the fixed names did not have. Only the within-process half
    // is checkable here — that two runs of the binary differ is the process
    // id, which one process cannot observe.
    let one = Scratch::new();
    let two = Scratch::new();
    assert_ne!(one.root, two.root);
    assert!(one.root.is_dir() && two.root.is_dir());

    let root = one.root.clone();
    std::fs::write(root.join("left-behind.mus"), b"x").unwrap();
    drop(one);
    assert!(!root.exists(), "scratch outlived the test that made it");
}

/// A schema exercising every field the image has to carry.
fn populate(engine: &Engine, session: &mut Session) {
    run(
        engine,
        session,
        "CREATE TABLE Parent (
            id INTEGER NOT NULL AUTO_INCREMENT,
            label VARCHAR(20) NOT NULL DEFAULT 'x' COMMENT 'the label',
            score DOUBLE NULL,
            kind ENUM('a', 'b', 'c') NULL,
            payload JSON NULL,
            madeAt DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
                ON UPDATE CURRENT_TIMESTAMP(3),
            shout VARCHAR(40) GENERATED ALWAYS AS (UPPER(label)) STORED,
            PRIMARY KEY (id),
            UNIQUE INDEX Parent_label_key (label),
            CONSTRAINT Parent_score_positive CHECK (score IS NULL OR score >= 0)
        )",
    );
    run(
        engine,
        session,
        "CREATE TABLE Child (
            id INTEGER NOT NULL,
            parentId INTEGER NOT NULL,
            blob0 BLOB NULL,
            PRIMARY KEY (id),
            CONSTRAINT Child_parent FOREIGN KEY (parentId)
                REFERENCES Parent(id) ON DELETE CASCADE ON UPDATE CASCADE
        )",
    );
    run(
        engine,
        session,
        "CREATE VIEW Loud AS SELECT id, shout FROM Parent",
    );
    run(
        engine,
        session,
        "INSERT INTO Parent (label, score, kind, payload) VALUES
            ('one', 1.5, 'a', '{\"k\": 1}'),
            ('two', NULL, NULL, NULL),
            ('日本', 0.0, 'c', '[]')",
    );
    run(
        engine,
        session,
        "INSERT INTO Child (id, parentId) VALUES (10, 1), (11, 2)",
    );
}

#[test]
fn an_image_round_trips_the_whole_catalog() {
    let (engine, mut session) = engine();
    populate(&engine, &mut session);

    let before = rows(
        &engine,
        &mut session,
        "SELECT id, label, score, kind, payload, shout FROM Parent ORDER BY id",
    );

    let scratch = Scratch::new();
    let path = scratch.path("round-trip.mus");
    engine.save_image(&path).unwrap();

    // A fresh engine that has never seen the DDL.
    let restored = Engine::new(ServerConfig::default());
    restored.load_image(&path).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());

    assert_eq!(
        rows(
            &restored,
            &mut session,
            "SELECT id, label, score, kind, payload, shout FROM Parent ORDER BY id"
        ),
        before
    );

    // The schema came back too, not just the rows: the constraints still bite.
    let err = restored
        .execute(
            &mut session,
            "INSERT INTO Child (id, parentId) VALUES (12, 99)",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1452, "foreign key survived the image");

    let err = restored
        .execute(
            &mut session,
            "INSERT INTO Parent (label) VALUES ('one')",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 1062, "unique index survived the image");

    let err = restored
        .execute(
            &mut session,
            "INSERT INTO Parent (label, score) VALUES ('four', -1)",
            &[],
        )
        .unwrap_err();
    assert_eq!(err.code, 3819, "CHECK survived the image");

    // AUTO_INCREMENT continues where it left off rather than restarting.
    run(
        &restored,
        &mut session,
        "INSERT INTO Parent (label) VALUES ('four')",
    );
    assert_eq!(
        rows(&restored, &mut session, "SELECT MAX(id) FROM Parent"),
        vec![vec!["4"]]
    );

    // The view is a view, not a table.
    assert_eq!(
        rows(
            &restored,
            &mut session,
            "SELECT shout FROM Loud ORDER BY id"
        )
        .len(),
        4
    );
}

#[test]
fn a_cascade_still_works_after_a_reload() {
    // Foreign key actions are stored as text and re-read; a lost ON DELETE
    // would leave orphans rather than reporting anything.
    let (engine, mut session) = engine();
    populate(&engine, &mut session);

    let scratch = Scratch::new();
    let path = scratch.path("cascade.mus");
    engine.save_image(&path).unwrap();

    let restored = Engine::new(ServerConfig::default());
    restored.load_image(&path).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());

    run(&restored, &mut session, "DELETE FROM Parent WHERE id = 1");
    assert_eq!(
        rows(&restored, &mut session, "SELECT id FROM Child ORDER BY id"),
        vec![vec!["11"]]
    );
}

#[test]
fn restoring_replaces_a_running_catalog() {
    // What a test suite uses between files: reset without a new process.
    let (engine, mut session) = engine();
    populate(&engine, &mut session);

    let scratch = Scratch::new();
    let path = scratch.path("restore.mus");
    run(
        &engine,
        &mut session,
        &format!("SELECT ultralight_snapshot('{}')", path.display()),
    );

    run(&engine, &mut session, "DELETE FROM Child");
    run(&engine, &mut session, "DELETE FROM Parent");
    run(&engine, &mut session, "DROP TABLE Child");
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Parent").len(),
        0
    );

    run(
        &engine,
        &mut session,
        &format!("SELECT ultralight_restore('{}')", path.display()),
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Parent").len(),
        3
    );
    assert_eq!(rows(&engine, &mut session, "SELECT id FROM Child").len(), 2);
}

#[test]
fn a_float_survives_exactly() {
    // Written as bits, not as text: a value that changes on reload is a wrong
    // answer, and 0.1 does not round trip through a decimal rendering.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE F (id INTEGER NOT NULL, v DOUBLE NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO F VALUES (1, 0.1), (2, 1e308), (3, -0.000000000000001)",
    );

    let before = rows(&engine, &mut session, "SELECT v FROM F ORDER BY id");
    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let decoded = image::decode(&encoded).unwrap();

    let restored = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    assert_eq!(
        rows(&restored, &mut session, "SELECT v FROM F ORDER BY id"),
        before
    );
}

#[test]
fn a_temporal_survives_as_a_temporal() {
    // The tag has to be in the image, not only in the row that was inserted.
    // Reloaded as text a DATE reads 2024 in arithmetic where MySQL says
    // 20240105 — the whole of #0054, reintroduced by a restart.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INTEGER NOT NULL, d DATE NOT NULL, dt DATETIME(3) NOT NULL, \
         tm TIME NOT NULL, s VARCHAR(30) NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES (1, '2024-01-05', '2024-01-05 06:07:08.500', '06:07:08', \
         '2024-01-05')",
    );

    let sql = "SELECT d + 0, dt + 0, tm + 0, s + 0, d FROM T ORDER BY id";
    let before = rows(&engine, &mut session, sql);
    // Stated rather than only compared, so a reload that lost the tag on both
    // sides could not pass by agreeing with itself.
    assert_eq!(
        before,
        vec![vec![
            "20240105",
            "20240105060708.500",
            "60708",
            "2024",
            "2024-01-05"
        ]]
    );

    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let decoded = image::decode(&encoded).unwrap();
    let restored = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    assert_eq!(rows(&restored, &mut session, sql), before);
}

#[test]
fn an_enum_survives_as_its_ordinal() {
    // The ordinal has to be in the image, not only in the row that was
    // inserted, and it cannot be recovered on the way back in: a value is
    // loaded before anything asks which column it belongs to. Reloaded as text
    // a member reads 0 in arithmetic where MySQL reads its position — the
    // whole of #0068, reintroduced by a restart.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INTEGER NOT NULL, e ENUM('a','b','c') NOT NULL, \
         r ENUM('b','a') NOT NULL, s VARCHAR(30) NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES (1, 'c', 'b', 'c'), (2, 'a', 'a', 'a')",
    );

    // The sort is in here as well as the arithmetic, because the two read
    // different halves of the value: `ORDER BY r` is the declared order and
    // `e + 0` is the ordinal, and an image that kept one could still lose the
    // other.
    let sql = "SELECT e, e + 0, s + 0, r FROM T ORDER BY r, id";
    let before = rows(&engine, &mut session, sql);
    // Stated rather than only compared, so a reload that lost the tag on both
    // sides could not pass by agreeing with itself.
    assert_eq!(
        before,
        vec![vec!["c", "3", "0", "b"], vec!["a", "1", "0", "a"]]
    );

    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let decoded = image::decode(&encoded).unwrap();
    let restored = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    assert_eq!(rows(&restored, &mut session, sql), before);
}

#[test]
fn a_truncated_image_is_refused() {
    let (engine, mut session) = engine();
    populate(&engine, &mut session);
    let encoded = image::encode(&engine.catalog_snapshot().unwrap());

    for cut in [0, 1, 4, 8, 32, encoded.len() / 2, encoded.len() - 1] {
        let err = image::decode(&encoded[..cut]).unwrap_err();
        assert_eq!(err.code, 1105, "cut at {cut}");
    }
}

#[test]
fn a_foreign_file_is_refused() {
    let err = image::decode(b"not an image at all").unwrap_err();
    assert!(
        err.message.contains("not a mysql-ultralight state image"),
        "{}",
        err.message
    );

    // An empty file is the common case: a bake step that failed part way.
    assert!(image::decode(&[]).is_err());
}

#[test]
fn an_image_from_another_format_version_is_refused() {
    let (engine, mut session) = engine();
    populate(&engine, &mut session);
    let mut encoded = image::encode(&engine.catalog_snapshot().unwrap());

    // Reading it anyway would not fail cleanly; it would produce a catalog with
    // the wrong shape and no complaint.
    encoded[4..8].copy_from_slice(&(image::FORMAT_VERSION + 1).to_le_bytes());
    let err = image::decode(&encoded).unwrap_err();
    assert!(err.message.contains("format version"), "{}", err.message);
}

#[test]
fn trailing_bytes_are_refused() {
    let (engine, mut session) = engine();
    populate(&engine, &mut session);
    let mut encoded = image::encode(&engine.catalog_snapshot().unwrap());
    encoded.push(0);

    let err = image::decode(&encoded).unwrap_err();
    assert!(err.message.contains("trailing"), "{}", err.message);
}

#[test]
fn a_missing_image_names_the_file() {
    let engine = Engine::new(ServerConfig::default());
    let err = engine
        .load_image(std::path::Path::new("/nonexistent/nope.mus"))
        .unwrap_err();
    assert!(err.message.contains("nope.mus"), "{}", err.message);
}

#[test]
fn a_snapshot_is_written_whole_or_not_at_all() {
    // Written to a temporary and renamed: a reader must never see a partial
    // image, because it would fail somewhere far from the bake that wrote it.
    let (engine, mut session) = engine();
    populate(&engine, &mut session);

    let scratch = Scratch::new();
    let path = scratch.path("atomic.mus");
    std::fs::write(&path, b"stale contents").unwrap();
    engine.save_image(&path).unwrap();

    let bytes = std::fs::read(&path).unwrap();
    assert!(image::decode(&bytes).is_ok());
    // Every file in the directory, not `atomic.tmp`: the temporary carries the
    // writing process's id, so naming one guessed spelling asserted about a
    // file that could never have existed under any behaviour.
    let left: Vec<_> = std::fs::read_dir(&scratch.root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "atomic.mus")
        .collect();
    assert!(left.is_empty(), "temporary left behind: {left:?}");
}

#[test]
fn binary_values_survive() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE B (id INTEGER NOT NULL, v BLOB NULL, PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO B VALUES (1, 'bytes')");

    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let decoded = image::decode(&encoded).unwrap();
    let table = decoded.database("testdb").unwrap().table("B").unwrap();
    assert_eq!(table.rows.len(), 1);
    assert!(matches!(
        &table.rows[0][1],
        Value::Bytes(b) if b == b"bytes"
    ));
}

#[test]
fn a_snapshot_inside_a_write_is_refused_rather_than_hanging() {
    // Writers hold a global gate across their copy-compute-commit cycle, and
    // it is not reentrant. Taking it again would wait forever on a lock this
    // thread already holds — and because the gate is global, every other
    // connection would wedge behind it too.
    let (engine, mut session) = engine();
    populate(&engine, &mut session);
    let scratch = Scratch::new();
    let path = scratch.path("nested.mus");

    for sql in [
        format!(
            "INSERT INTO Child (id, parentId, blob0) VALUES (99, 1, ultralight_snapshot('{}'))",
            path.display()
        ),
        format!(
            "UPDATE Child SET blob0 = (SELECT ultralight_restore('{}'))",
            path.display()
        ),
        format!(
            "DELETE FROM Child WHERE ultralight_snapshot('{}') = 1",
            path.display()
        ),
    ] {
        let err = engine.execute(&mut session, &sql, &[]).unwrap_err();
        assert_eq!(err.code, 1105, "for: {sql}");
    }

    // The server is still usable: the gate was released, not leaked.
    run(
        &engine,
        &mut session,
        "INSERT INTO Child (id, parentId) VALUES (12, 1)",
    );
    assert_eq!(rows(&engine, &mut session, "SELECT id FROM Child").len(), 3);
}

#[test]
fn a_snapshot_or_restore_inside_a_transaction_is_refused() {
    // A snapshot would bake rows a rollback then removes from the catalog but
    // not from the file; a restore would leave the rollback replaying
    // pre-transaction tables over a catalog that has been replaced wholesale.
    let (engine, mut session) = engine();
    populate(&engine, &mut session);

    let scratch = Scratch::new();
    let path = scratch.path("txn.mus");
    run(
        &engine,
        &mut session,
        &format!("SELECT ultralight_snapshot('{}')", path.display()),
    );

    run(&engine, &mut session, "START TRANSACTION");
    run(
        &engine,
        &mut session,
        "INSERT INTO Child (id, parentId) VALUES (77, 1)",
    );
    for name in ["ultralight_snapshot", "ultralight_restore"] {
        let err = engine
            .execute(
                &mut session,
                &format!("SELECT {name}('{}')", path.display()),
                &[],
            )
            .unwrap_err();
        assert_eq!(err.code, 1105, "{name} inside a transaction");
    }
    run(&engine, &mut session, "ROLLBACK");

    // The rollback did what it always does, undisturbed.
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Child ORDER BY id").len(),
        2
    );
}

#[test]
fn describing_never_snapshots_restores_or_takes_a_lock() {
    // `describe` runs the statement to learn its shape, so a SELECT that is
    // not free of consequences must not have those consequences: preparing it
    // would write a file, replace the catalog, or take a lock nobody asked for.
    let (engine, mut session) = engine();
    populate(&engine, &mut session);

    let scratch = Scratch::new();
    let path = scratch.path("describe.mus");
    run(
        &engine,
        &mut session,
        &format!("SELECT ultralight_snapshot('{}')", path.display()),
    );

    // A row the image does not have, so a restore would be visible.
    run(
        &engine,
        &mut session,
        "INSERT INTO Parent (label) VALUES ('after the snapshot')",
    );
    let live = rows(&engine, &mut session, "SELECT id FROM Parent ORDER BY id");

    let written = scratch.path("describe-never.mus");

    // Where the call is on the path to a column's shape, it is reached and
    // refused, and the statement describes as nothing.
    for sql in [
        format!("SELECT ultralight_restore('{}')", path.display()),
        format!("SELECT ultralight_snapshot('{}')", written.display()),
        format!(
            "WITH c AS (SELECT ultralight_restore('{}') AS r) SELECT r FROM c",
            path.display()
        ),
        "SELECT GET_LOCK('describe-probe', 0)".to_owned(),
    ] {
        assert!(
            engine.describe(&session, &sql, 0).unwrap().is_none(),
            "described something with consequences: {sql}"
        );
    }

    // In a WHERE clause it is not reached at all, because describing reads the
    // schema and not the rows — so there is nothing to refuse, and a shape
    // comes back. What must still be true is the same thing: that describing
    // it changed nothing, which the assertions below check.
    let shaped = engine
        .describe(
            &session,
            &format!(
                "SELECT id FROM Parent WHERE ultralight_restore('{}') = 1",
                path.display()
            ),
            0,
        )
        .unwrap();
    assert_eq!(
        shaped.map(|columns| columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>()),
        Some(vec!["id".to_owned()])
    );

    // Nothing was restored, nothing was written, and the lock is free.
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Parent ORDER BY id"),
        live
    );
    assert!(!written.exists(), "describe wrote an image");
    assert_eq!(
        rows(
            &engine,
            &mut session,
            "SELECT IS_FREE_LOCK('describe-probe')"
        ),
        vec![vec!["1"]]
    );

    // The image is still good, and restoring it properly still works.
    run(
        &engine,
        &mut session,
        &format!("SELECT ultralight_restore('{}')", path.display()),
    );
    assert_eq!(
        rows(&engine, &mut session, "SELECT id FROM Parent").len(),
        3
    );
}

#[test]
fn describing_does_not_move_a_generated_value() {
    // UUID and RAND advance a counter on the engine, not the session, so a
    // probe would consume one — and the same test would produce different ids
    // depending only on whether the client used a prepared statement.
    let (engine, mut session) = engine();

    assert!(engine
        .describe(&session, "SELECT UUID()", 0)
        .unwrap()
        .is_none());
    assert!(engine
        .describe(&session, "SELECT RAND()", 0)
        .unwrap()
        .is_none());

    let (other, mut other_session) = fresh_engine();
    assert_eq!(
        rows(&engine, &mut session, "SELECT UUID()"),
        rows(&other, &mut other_session, "SELECT UUID()")
    );
}

#[test]
fn a_failed_snapshot_leaves_nothing_behind() {
    let (engine, mut session) = engine();
    populate(&engine, &mut session);

    let scratch = Scratch::new();
    // A snapshot onto a directory: the write cannot succeed, and the question
    // is what it leaves behind when it does not.
    let directory = scratch.path("as-a-directory.mus");
    std::fs::create_dir_all(&directory).unwrap();

    let err = engine.save_image(&directory).unwrap_err();
    // The error names the file that was asked for, not a temporary the caller
    // has never heard of.
    assert!(
        err.message.contains("as-a-directory.mus") && !err.message.contains(".tmp"),
        "{}",
        err.message
    );
    // Only this test's own directory is searched. Scanning the shared temp
    // directory would report another run's temporary as this run's leak.
    let leftovers: Vec<_> = std::fs::read_dir(&scratch.root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
}

#[test]
fn describing_refuses_consequences_hidden_inside_a_view() {
    // The refusal is at the point of the call, not a scan of the statement,
    // because `SELECT * FROM v` says nothing about what `v` is defined as.
    let (engine, mut session) = engine();
    populate(&engine, &mut session);

    let scratch = Scratch::new();
    let written = scratch.path("via-view.mus");
    run(
        &engine,
        &mut session,
        &format!(
            "CREATE VIEW Sneaky AS SELECT ultralight_snapshot('{}') AS taken",
            written.display()
        ),
    );

    // Creating the view runs its definition once, to fix its shape — so the
    // file exists already, and the question is whether *describing* writes it
    // again.
    std::fs::remove_file(&written).ok();

    assert!(
        engine
            .describe(&session, "SELECT taken FROM Sneaky", 0)
            .unwrap()
            .is_none(),
        "described a view that writes"
    );
    assert!(!written.exists(), "describing a view wrote an image");

    // Selecting from it still works: only the probe is restricted.
    run(&engine, &mut session, "SELECT taken FROM Sneaky");
    assert!(written.exists());
}

#[test]
fn a_document_survives_as_a_document() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (id INTEGER NOT NULL, doc JSON NULL, txt VARCHAR(20) NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        r#"INSERT INTO D VALUES (1, '{"a":[1,2]}', '[9,9]')"#,
    );

    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let decoded = image::decode(&encoded).unwrap();
    let table = decoded.database("testdb").unwrap().table("D").unwrap();

    // The tag is the difference between a document and a string that looks
    // like one, so an image that dropped it would reload a catalog whose JSON
    // columns nest as strings — a wrong answer a reload away from its cause.
    assert!(
        matches!(&table.rows[0][1], Value::Json(s) if s == r#"{"a": [1, 2]}"#),
        "{:?}",
        table.rows[0][1]
    );
    assert!(
        matches!(&table.rows[0][2], Value::Text(s, _) if s == "[9,9]"),
        "{:?}",
        table.rows[0][2]
    );

    // And it still nests correctly after the round trip.
    let reloaded = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    assert_eq!(
        rows(
            &reloaded,
            &mut session,
            "SELECT JSON_OBJECT('w', doc), JSON_OBJECT('w', txt) FROM D"
        ),
        vec![vec![r#"{"w": {"a": [1, 2]}}"#, r#"{"w": "[9,9]"}"#]]
    );
}

#[test]
fn an_image_whose_document_is_not_a_document_is_refused() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (id INTEGER NOT NULL, doc JSON NULL, PRIMARY KEY (id))",
    );
    run(&engine, &mut session, "INSERT INTO D VALUES (1, '[111]')");

    let mut encoded = image::encode(&engine.catalog_snapshot().unwrap());

    // Corrupted in place, so every later offset still lines up and the only
    // thing wrong with the file is the document itself.
    let at = encoded
        .windows(5)
        .position(|w| w == b"[111]")
        .expect("the document is in the image");
    encoded[at..at + 5].copy_from_slice(b"[111 ");

    let err = image::decode(&encoded).unwrap_err();
    assert_eq!(err.code, 1105, "{err}");
    assert!(
        err.message.contains("tagged JSON"),
        "the error should name what is wrong: {err}"
    );

    // A value that is tagged JSON is therefore always a document, which is
    // what lets every reader of one parse without a fallback.
    let good = image::encode(&engine.catalog_snapshot().unwrap());
    assert!(image::decode(&good).is_ok());
}

#[test]
fn a_view_keeps_its_column_types_through_an_image() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Base (id INT NOT NULL, name VARCHAR(40) NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE VIEW Mirror AS SELECT id, name FROM Base",
    );
    run(&engine, &mut session, "INSERT INTO Base VALUES (1, 'a')");

    let described = rows(
        &engine,
        &mut session,
        "SELECT COLUMN_NAME, COLUMN_TYPE, NUMERIC_PRECISION, CHARACTER_MAXIMUM_LENGTH
         FROM information_schema.columns
         WHERE TABLE_NAME = 'Mirror' ORDER BY ORDINAL_POSITION",
    );
    assert_eq!(
        described,
        vec![
            vec!["id", "int", "10", "NULL"],
            vec!["name", "varchar(40)", "NULL", "40"]
        ]
    );

    // The shape is stored with the view, so a booted image has to carry it —
    // an image is how a test database starts, and a view described differently
    // after a reload is a schema that drifts on restart.
    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let reloaded =
        Engine::from_catalog(image::decode(&encoded).unwrap(), ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    assert_eq!(
        rows(
            &reloaded,
            &mut session,
            "SELECT COLUMN_NAME, COLUMN_TYPE, NUMERIC_PRECISION, CHARACTER_MAXIMUM_LENGTH
             FROM information_schema.columns
             WHERE TABLE_NAME = 'Mirror' ORDER BY ORDINAL_POSITION"
        ),
        described
    );
    assert_eq!(
        rows(&reloaded, &mut session, "SELECT id, name FROM Mirror"),
        vec![vec!["1", "a"]]
    );
}

#[test]
fn an_older_image_s_documents_are_canonicalised_on_load() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (id INT NOT NULL, doc JSON NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        r#"INSERT INTO D VALUES (1, '{"a": 1}')"#,
    );

    // An image written before documents were stored in MySQL's own form holds
    // whatever text was typed. Forged here by rewriting the value in place,
    // because the point is what happens to a file this build did not write.
    let mut encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let at = encoded
        .windows(8)
        .position(|w| w == br#"{"a": 1}"#)
        .expect("the document is in the image");
    encoded[at..at + 8].copy_from_slice(br#"{"a":1} "#);

    // Reading it back gives the canonical form, so the same data cannot read
    // one way through an INSERT and another through a reload.
    let decoded = image::decode(&encoded).unwrap();
    let table = decoded.database("testdb").unwrap().table("D").unwrap();
    assert!(
        matches!(&table.rows[0][1], Value::Json(s) if s == r#"{"a": 1}"#),
        "{:?}",
        table.rows[0][1]
    );
}

#[test]
fn a_boolean_never_reaches_the_image_and_is_written_as_its_integer_if_it_does() {
    // Why #0061 costs no format version. Every value entering a row goes
    // through `SqlType::coerce`, and no column type it knows how to build is a
    // boolean — MySQL agrees, because `BOOLEAN` there is a `TINYINT(1)` and
    // `JSON_ARRAY(b)` over one holding 1 is `[1]`, not `[true]`.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INTEGER NOT NULL, f BOOLEAN, t TINYINT(1), n INTEGER, \
         v VARCHAR(10), b BLOB, d DECIMAL(4,2), g ENUM('2','1'), PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES (1, TRUE, TRUE, TRUE, TRUE, TRUE, TRUE, TRUE)",
    );

    let mut catalog = engine.catalog_snapshot().unwrap();
    let stored = &catalog.database("testdb").unwrap().table("T").unwrap().rows[0];
    assert!(
        !stored.iter().any(|v| matches!(v, Value::Bool(_))),
        "a boolean reached a row: {stored:?}"
    );

    // Put one there anyway and encode it, because "cannot happen" is a claim
    // about today's `coerce` and the writer has to have an answer either way.
    // It writes the integer under the tag the column would already have used,
    // so nothing is lost and no reader has to learn a tag for it.
    catalog
        .database_mut("testdb")
        .unwrap()
        .table_mut("T")
        .unwrap()
        .rows[0][1] = Value::Bool(true);
    let decoded = image::decode(&image::encode(&catalog)).unwrap();
    assert_eq!(
        decoded.database("testdb").unwrap().table("T").unwrap().rows[0][1],
        Value::Int(1)
    );

    // And the boolean did not move the version: 7 was #0068's ENUM tag, 8 was
    // #0069's unsigned one, 9 is #0126's binary collation, 10 is the column
    // collation #0129/#0134/#0152 needed, 11 is #0121's decimal mark inside a
    // document and 12 is #0205's foreign key schema, none of them this. Pinned
    // rather than derived, so the day something *does* give a boolean a tag of
    // its own it has to come back here and say so.
    assert_eq!(image::FORMAT_VERSION, 12);
}

#[test]
fn an_unsigned_column_survives_at_its_full_width_and_stays_unsigned() {
    // Both halves of the tag have to make it into the image. The *magnitude*,
    // because a `BIGINT UNSIGNED` reaches 18446744073709551615 and a v7 image
    // had no field that could hold it; and the *sign*, because an unsigned
    // column reloaded signed answers `u - 100` with -93 where MySQL raises
    // 1690 — the whole of #0069, reintroduced by a restart.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE T (id INTEGER NOT NULL, u BIGINT UNSIGNED NOT NULL,          i INTEGER UNSIGNED NOT NULL, s BIGINT NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO T VALUES (1, 18446744073709551615, 7, 7)",
    );

    // The subtraction is in here beside the value because the two read
    // different halves of the tag: an image that kept the digits and lost the
    // sign would still print 18446744073709551615 and would stop refusing.
    let sql = "SELECT u, u - 1, s - 100 FROM T";
    let before = rows(&engine, &mut session, sql);
    // Stated rather than only compared, so a reload that lost the tag on both
    // sides could not pass by agreeing with itself.
    assert_eq!(
        before,
        vec![vec!["18446744073709551615", "18446744073709551614", "-93"]]
    );

    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let decoded = image::decode(&encoded).unwrap();
    let restored = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    assert_eq!(rows(&restored, &mut session, sql), before);

    let err = restored
        .execute(&mut session, "SELECT i - 100 FROM T", &[])
        .unwrap_err();
    assert_eq!(err.code, 1690, "{}", err.message);
}

#[test]
fn a_reloaded_document_keeps_a_decimal_s_scale() {
    // A document is stored as its text and re-read on load, so the reload is
    // one more place the scale can be dropped — and it is not the same parse
    // as reading a *string* into a JSON column: this text is already a
    // document, and its `2.50` is the decimal it was built from rather than
    // the double the same digits would spell arriving from outside.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (id INT NOT NULL, amount DECIMAL(10,2) NOT NULL, doc JSON NOT NULL, \
                         PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO D (id, amount, doc) VALUES (1, '2.50', '{}')",
    );
    run(
        &engine,
        &mut session,
        "UPDATE D SET doc = JSON_OBJECT('a', amount) WHERE id = 1",
    );

    let sql = "SELECT doc, doc->'$.a' FROM D";
    let before = rows(&engine, &mut session, sql);
    // Stated rather than only compared, so a reload that dropped the scale on
    // both sides could not pass by agreeing with itself.
    assert_eq!(before, vec![vec![r#"{"a": 2.50}"#, "2.50"]]);

    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let decoded = image::decode(&encoded).unwrap();
    let restored = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    assert_eq!(rows(&restored, &mut session, sql), before);
}

#[test]
fn a_document_holding_serde_s_number_key_still_reloads() {
    // The worst shape this can take. A `Value::Json` is re-read on load, and
    // a row that fails to parse there is not one bad row — `decode` refuses
    // the whole image, so a single legal document would cost the entire
    // saved state. `$serde_json::private::Number` is a key like any other on
    // 8.3, and this engine's own JSON_OBJECT can build one.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE K (id INT NOT NULL, doc JSON NOT NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO K VALUES (1, '{\"$serde_json::private::Number\": \"1\"}'), \
                              (2, '{\"$serde_json::private::Number\": 1}'), \
                              (3, '[2.50, {\"$serde_json::private::Number\": \"1\"}]')",
    );

    let sql = "SELECT id, doc, JSON_TYPE(doc) FROM K ORDER BY id";
    let before = rows(&engine, &mut session, sql);
    // Stated rather than only compared, so a reload that mangled both sides
    // could not pass by agreeing with itself.
    assert_eq!(
        before,
        vec![
            vec!["1", r#"{"$serde_json::private::Number": "1"}"#, "OBJECT"],
            vec!["2", r#"{"$serde_json::private::Number": 1}"#, "OBJECT"],
            vec![
                "3",
                r#"[2.5, {"$serde_json::private::Number": "1"}]"#,
                "ARRAY"
            ],
        ]
    );

    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let decoded = image::decode(&encoded).expect("the image loads");
    let restored = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    assert_eq!(rows(&restored, &mut session, sql), before);
}

/// A binary-collated column keeps its collation across a save and a reload.
///
/// No DDL `COLLATE` clause can declare one — only a `_ci` collation is
/// accepted — so the column that carries one here comes from
/// `CREATE TABLE ... AS SELECT` off `information_schema`, which is exactly
/// what 8.3 does: the copy of
/// `COLUMN_TYPE` is `utf8mb3_bin` there too. The collation is the only thing
/// that puts the BINARY flag on a text column's wire definition, so an image
/// that dropped it would make the same table describe itself one way before a
/// snapshot and another way after — with nothing in the rows to show for it.
#[test]
fn a_binary_collation_survives_a_reload() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE Copied AS SELECT COLUMN_TYPE, COLUMN_NAME \
         FROM information_schema.columns WHERE TABLE_NAME = 'Copied'",
    );

    let binary_flags = |engine: &Engine, session: &Session| -> Vec<bool> {
        engine
            .describe(session, "SELECT * FROM Copied", 0)
            .unwrap()
            .unwrap()
            .iter()
            .map(|c| c.to_wire().flags & mysql_ultralight_protocol::column::flags::BINARY != 0)
            .collect()
    };
    // The first column carries the collation and the second does not, which is
    // what makes this a round-trip test rather than a constant.
    let before = binary_flags(&engine, &session);
    assert_eq!(before, vec![true, false]);

    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let decoded = image::decode(&encoded).unwrap();
    let restored = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    assert_eq!(binary_flags(&restored, &session), before);
}

/// A name is written to the image as it was written in the DDL.
///
/// The image is the one place a wrong name becomes *permanent*: the format is
/// append-only, so an image baked with `` `we``ird` `` stored as ``we``ird``
/// and `` COLLATE `utf8mb4_general_ci` `` stored as `` `utf8mb4_general_ci` ``
/// carries both into every process that boots from it. Nothing on load can undo either —
/// a table genuinely named ``we``ird`` is legal — so the only place to be
/// right is where the name is read out of the statement.
#[test]
fn a_quoted_name_survives_a_reload_as_it_was_written() {
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE `we``ird` (`c``ol` INT)",
    );
    run(&engine, &mut session, "CREATE TABLE `my.table` (a INT)");
    run(&engine, &mut session, "INSERT INTO `my.table` VALUES (1)");
    run(
        &engine,
        &mut session,
        "CREATE TABLE C (a VARCHAR(10) COLLATE `utf8mb4_general_ci`)",
    );

    let encoded = image::encode(&engine.catalog_snapshot().unwrap());
    let decoded = image::decode(&encoded).expect("the image loads");
    let restored = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());

    assert_eq!(
        rows(
            &restored,
            &mut session,
            "SELECT TABLE_NAME, COLUMN_NAME, COLLATION_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = 'testdb' ORDER BY TABLE_NAME, COLUMN_NAME",
        ),
        // NULL on the two INT columns, because only a column of characters
        // has a collation — which is what 8.3 reports for an INT too.
        vec![
            vec!["C", "a", "utf8mb4_general_ci"],
            vec!["my.table", "a", "NULL"],
            vec!["we`ird", "c`ol", "NULL"],
        ]
    );
    // The reloaded name is one a statement can still address, which is the
    // half a metadata assertion alone would miss.
    assert_eq!(
        rows(&restored, &mut session, "SELECT a FROM `my.table`"),
        vec![vec!["1"]]
    );
}

#[test]
fn a_decimal_inside_a_document_survives_the_image_as_a_decimal() {
    // #0121's half of the format bump. The type of a number in a document is
    // carried in the text a `Value::Json` holds, so an image that wrote the
    // text MySQL sends instead of the text the engine stores would reload
    // every decimal in every document as a double — the bug this closes,
    // reintroduced by a restart and nowhere near its cause.
    let (engine, mut session) = engine();
    run(
        &engine,
        &mut session,
        "CREATE TABLE D (id INT NOT NULL, v DECIMAL(10,1) NULL, doc JSON NULL, PRIMARY KEY (id))",
    );
    run(
        &engine,
        &mut session,
        "INSERT INTO D (id, v) VALUES (1, 2.5)",
    );
    run(
        &engine,
        &mut session,
        "UPDATE D SET doc = JSON_OBJECT('v', v)",
    );
    // The digits are the same either way — `DECIMAL(10,1)` 2.5 renders `2.5`
    // and so does the double — so only the type below can tell the round trip
    // worked.
    run(
        &engine,
        &mut session,
        "INSERT INTO D (id, doc) VALUES (2, '{\"v\": 2.5}')",
    );

    let decoded = image::decode(&image::encode(&engine.catalog_snapshot().unwrap())).unwrap();
    let restored = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());

    assert_eq!(
        rows(
            &restored,
            &mut session,
            "SELECT id, doc, JSON_TYPE(JSON_EXTRACT(doc, '$.v')) FROM D ORDER BY id"
        ),
        vec![
            vec!["1", r#"{"v": 2.5}"#, "DECIMAL"],
            vec!["2", r#"{"v": 2.5}"#, "DOUBLE"],
        ]
    );
}

/// v12: a foreign key's parent is two fields, and both survive.
///
/// The database the constraint wrote is held apart from the parent's name
/// because the join cannot be undone — `REFERENCES app.P(id)` and
/// ``REFERENCES `app.P`(id)`` render to the same `app.P`, and a reader
/// splitting that turns a table actually called `app.P` into a table `P` in a
/// database `app`. 8.3 reports the first as `app`/`P` and the second as this
/// database and `app.P`, measured, so a v11 image genuinely cannot hold the
/// difference.
///
/// Through the image and not only through the catalog, because the field is
/// what the version was bumped for: a schema written and not read back would
/// reload as the unqualified case and answer both columns wrongly.
#[test]
fn a_foreign_keys_parent_keeps_its_database_and_its_name_apart() {
    let (engine, mut session) = engine();
    engine.bootstrap_database("appdb").unwrap();
    run(&engine, &mut session, "CREATE TABLE P (id INT PRIMARY KEY)");
    run(
        &engine,
        &mut session,
        "CREATE TABLE appdb.X (id INT PRIMARY KEY)",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE `a.b` (id INT PRIMARY KEY)",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE Q (pid INT, CONSTRAINT fkq FOREIGN KEY (pid) REFERENCES testdb.P (id))",
    );
    // A parent in *another* database, which is the only shape where the stored
    // schema is not the child's own — and so the only one that can tell a
    // field that round trips from one that is re-derived on the way in.
    run(
        &engine,
        &mut session,
        "CREATE TABLE A (pid INT, CONSTRAINT fka FOREIGN KEY (pid) REFERENCES appdb.X (id))",
    );
    run(
        &engine,
        &mut session,
        "CREATE TABLE Dot (pid INT, CONSTRAINT fkd FOREIGN KEY (pid) REFERENCES `a.b` (id))",
    );

    let read = "SELECT CONSTRAINT_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME \
                FROM information_schema.key_column_usage \
                WHERE CONSTRAINT_SCHEMA = 'testdb' AND REFERENCED_TABLE_NAME IS NOT NULL \
                ORDER BY CONSTRAINT_NAME";
    let expected = vec![
        vec!["fka", "appdb", "X"],
        // The dotted name is a name and not a qualification, so the schema is
        // the child's own.
        vec!["fkd", "testdb", "a.b"],
        vec!["fkq", "testdb", "P"],
    ];
    assert_eq!(rows(&engine, &mut session, read), expected);

    let decoded = image::decode(&image::encode(&engine.catalog_snapshot().unwrap())).unwrap();
    let restored = Engine::from_catalog(decoded, ServerConfig::default()).unwrap();
    let mut session = Session::new(ServerConfig::default(), 1);
    session.current_database = Some("testdb".to_owned());
    assert_eq!(rows(&restored, &mut session, read), expected);

    // And the dotted parent is still the parent it names, which is the half
    // `information_schema` cannot show: a key resolved by splitting would look
    // for a table `b` in a database `a` and find nothing, so every write would
    // be waved through.
    let err = restored
        .execute(&mut session, "INSERT INTO Dot VALUES (7)", &[])
        .unwrap_err();
    assert_eq!(err.code, 1452, "{err}");
}
