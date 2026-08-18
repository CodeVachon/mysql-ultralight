//! Concurrent writers, which is how a connection pool uses this engine.

use std::sync::Arc;

use mysql_ultralight_engine::{Engine, Outcome, ServerConfig, Session};

fn session(engine: &Engine, id: u32) -> Session {
    let _ = engine;
    let mut session = Session::new(ServerConfig::default(), id);
    session.current_database = Some("testdb".to_owned());
    session
}

/// Writers compute against a copy of the database and swap it in, because they
/// cannot hold the catalog's write lock while evaluating a subquery. Without a
/// gate serialising that cycle, two connections each discard the other's rows
/// and both report success — silent data loss under an ordinary Prisma pool.
#[test]
fn concurrent_inserts_all_land() {
    let engine = Engine::new(ServerConfig::default());
    engine.bootstrap_database("testdb").unwrap();
    let mut setup = session(&engine, 0);
    engine
        .execute(
            &mut setup,
            "CREATE TABLE Counted (id INTEGER NOT NULL, PRIMARY KEY (id))",
            &[],
        )
        .unwrap();

    let threads: Vec<_> = (0..4u32)
        .map(|worker| {
            let engine = Arc::clone(&engine);
            std::thread::spawn(move || {
                let mut session = session(&engine, worker + 1);
                for row in 0..200 {
                    let id = worker * 1000 + row;
                    engine
                        .execute(
                            &mut session,
                            &format!("INSERT INTO Counted (id) VALUES ({id})"),
                            &[],
                        )
                        .unwrap_or_else(|e| panic!("insert {id}: {e}"));
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }

    let Outcome::Rows(rows) = engine
        .execute(&mut setup, "SELECT COUNT(*) FROM Counted", &[])
        .unwrap()
    else {
        panic!("expected rows")
    };
    assert_eq!(rows.rows[0][0].as_text(), "800");
}

/// The same hazard across two tables: the copy is of the whole database, so
/// writers that never touch the same table still overwrite each other.
#[test]
fn concurrent_writes_to_different_tables_all_land() {
    let engine = Engine::new(ServerConfig::default());
    engine.bootstrap_database("testdb").unwrap();
    let mut setup = session(&engine, 0);
    for table in ["A", "B"] {
        engine
            .execute(
                &mut setup,
                &format!("CREATE TABLE {table} (id INTEGER NOT NULL, PRIMARY KEY (id))"),
                &[],
            )
            .unwrap();
    }

    let threads: Vec<_> = ["A", "B"]
        .iter()
        .enumerate()
        .map(|(worker, table)| {
            let engine = Arc::clone(&engine);
            let table = table.to_string();
            std::thread::spawn(move || {
                let mut session = session(&engine, worker as u32 + 1);
                for id in 0..300 {
                    engine
                        .execute(
                            &mut session,
                            &format!("INSERT INTO {table} (id) VALUES ({id})"),
                            &[],
                        )
                        .unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }

    for table in ["A", "B"] {
        let Outcome::Rows(rows) = engine
            .execute(&mut setup, &format!("SELECT COUNT(*) FROM {table}"), &[])
            .unwrap()
        else {
            panic!("expected rows")
        };
        assert_eq!(rows.rows[0][0].as_text(), "300", "{table}");
    }
}
