//! What `COM_STMT_PREPARE` declares, held against what execution sends.
//!
//! Describing a statement reads the schema and not the rows, which is what
//! stops preparing a query from costing as much as running it. That only works
//! while every column type is derived from the expression and the input
//! schema rather than from a value — so this runs each shape both ways and
//! insists the two agree. A type inferred from the first row would pass every
//! other test in the suite and fail here.

use mysql_ultralight_engine::{Engine, Outcome, ServerConfig, Session, Value};

fn setup() -> (std::sync::Arc<Engine>, Session) {
    let engine = Engine::new(ServerConfig::default());
    engine.bootstrap_database("t").unwrap();
    let mut s = Session::new(ServerConfig::default(), 1);
    s.current_database = Some("t".to_owned());
    for sql in [
        "CREATE TABLE A (id INT NOT NULL AUTO_INCREMENT, k INT, name VARCHAR(40), amt DECIMAL(10,2), body TEXT, kind ENUM('x','y'), at DATETIME(3), flag BOOLEAN, j JSON, PRIMARY KEY (id))",
        "CREATE TABLE B (id INT NOT NULL, aid INT, label VARCHAR(20), PRIMARY KEY (id))",
        "INSERT INTO A (k, name, amt, body, kind, at, flag, j) VALUES (1,'one',1.50,'b1','x','2026-01-01 00:00:00.000',1,'{\"a\":1}'), (2,'two',2.50,'b2','y','2026-02-02 00:00:00.000',0,'[1,2]'), (2,'three',3.50,'b3','x','2026-03-03 00:00:00.000',1,'null')",
        "INSERT INTO B VALUES (1,1,'p'),(2,2,'q'),(3,9,'r')",
        "CREATE VIEW V AS SELECT id, name, k FROM A",
    ] {
        engine.execute(&mut s, sql, &[]).unwrap();
    }
    (engine, s)
}

#[test]
fn describe_matches_execute() {
    let (engine, mut s) = setup();
    let queries = [
        "SELECT * FROM A",
        "SELECT id, name FROM A",
        "SELECT A.*, B.label FROM A JOIN B ON B.aid = A.id",
        "SELECT a.id, b.name FROM A a JOIN A b ON a.k = b.k",
        "SELECT id FROM A LEFT JOIN B ON B.aid = A.id",
        "SELECT COUNT(*), SUM(k), MIN(name), MAX(amt), AVG(k) FROM A",
        "SELECT k, COUNT(*) c FROM A GROUP BY k",
        "SELECT k, COUNT(*) c FROM A GROUP BY k HAVING COUNT(*) > 1",
        "SELECT id, ROW_NUMBER() OVER (ORDER BY id) rn FROM A",
        "SELECT id, SUM(k) OVER (PARTITION BY k ORDER BY id) s FROM A",
        "SELECT id, LAG(k, 1, 0) OVER (ORDER BY id) l FROM A",
        "SELECT * FROM V",
        "SELECT V.name FROM V JOIN B ON B.aid = V.id",
        "SELECT * FROM (SELECT id, name FROM A) d",
        "WITH c AS (SELECT id, k FROM A) SELECT c.id, c.k FROM c",
        "SELECT id, (SELECT MAX(k) FROM A) m FROM A",
        "SELECT id FROM A WHERE id IN (SELECT aid FROM B)",
        "SELECT id, name FROM A UNION SELECT id, label FROM B",
        "SELECT id FROM A UNION ALL SELECT id FROM B",
        "SELECT id FROM A ORDER BY name DESC LIMIT 2",
        "SELECT DISTINCT k FROM A",
        "SELECT 1, 'x', NULL, 1.5, TRUE",
        "SELECT CONCAT(name, '!') n, UPPER(name) u, LENGTH(body) l FROM A",
        "SELECT CAST(k AS CHAR) c, CAST(name AS UNSIGNED) u FROM A",
        "SELECT DATE_FORMAT(at, '%Y') y, YEAR(at) yy, DATEDIFF(at, at) d FROM A",
        "SELECT DATE_ADD(at, INTERVAL 1 DAY) d FROM A",
        "SELECT JSON_EXTRACT(j, '$.a') e, j->>'$.a' q FROM A",
        "SELECT CASE WHEN k > 1 THEN 'big' ELSE 'small' END c FROM A",
        "SELECT COALESCE(name, 'none') c, IFNULL(k, 0) i FROM A",
        "SELECT k + 1 a, k * 2 b, k / 2 c, k % 2 d FROM A",
        "SELECT kind, flag, amt, at, j FROM A",
        "SELECT GROUP_CONCAT(name) g FROM A",
        "SELECT * FROM A WHERE 1 = 0",
        "SELECT COUNT(*) FROM A WHERE 1 = 0",
        "SELECT id FROM A a WHERE EXISTS (SELECT 1 FROM B WHERE B.aid = a.id)",
        "SELECT TABLE_NAME FROM information_schema.tables WHERE TABLE_SCHEMA = 't'",
        "SELECT COLUMN_NAME, DATA_TYPE FROM information_schema.columns WHERE TABLE_NAME = 'A'",
        "SELECT ST_Distance_Sphere(POINT(0,0), POINT(0,1)) d",
        "SELECT id FROM A ORDER BY id",
        "SELECT b.label, a.name FROM B b LEFT JOIN A a ON a.id = b.aid",
        "SELECT * FROM A CROSS JOIN B",
        "SELECT id, name FROM A WHERE name LIKE 'o%'",
        "SELECT jt.n FROM A, JSON_TABLE(A.j, '$[*]' COLUMNS(n INT PATH '$.n')) jt",
        "SELECT A.id, jt.n FROM A LEFT JOIN JSON_TABLE(A.j, '$[*]' COLUMNS(n INT PATH '$.n')) jt ON 1 = 1",
        "VALUES ROW(1, 'a'), ROW(2, 'b')",
    ];

    let mut mismatches = Vec::new();
    let mut compared = 0;
    for q in queries {
        let described = engine.describe(&s, q, 0);
        // Every shape has to run. One that stops executing — a typo, or a
        // regression that makes it error — would otherwise drop out of the
        // comparison silently and shrink this test without failing it.
        let executed = match engine.execute(&mut s, q, &[]) {
            Ok(Outcome::Rows(r)) => r.columns,
            Ok(other) => {
                mismatches.push(format!("returned no rows ({other:?}): {q}"));
                continue;
            }
            Err(e) => {
                mismatches.push(format!("execute failed {} {}: {q}", e.code, e.message));
                continue;
            }
        };
        compared += 1;
        match described {
            None => mismatches.push(format!("described nothing: {q}")),
            Some(d) => {
                let f = |c: &[mysql_ultralight_engine::OutColumn]| {
                    c.iter()
                        .map(|c| format!("{}:{:?}:{}", c.name, c.ty, c.nullable))
                        .collect::<Vec<_>>()
                };
                if f(&d) != f(&executed) {
                    mismatches.push(format!(
                        "{q}\n    described {:?}\n    executed  {:?}",
                        f(&d),
                        f(&executed)
                    ));
                }
            }
        }
    }
    for m in &mismatches {
        println!("MISMATCH {m}");
    }
    assert!(mismatches.is_empty(), "{} mismatches", mismatches.len());
    assert_eq!(compared, queries.len(), "some shapes never ran");
}

/// A statement is described with every parameter bound to NULL, because its
/// shape does not depend on its arguments. Where the parameter *is* the whole
/// column, that is visible: `SELECT ?` describes as a string and executes as
/// whatever was bound. MySQL declares `VAR_STRING` at prepare time too, so the
/// declared type is right; what differs is the name, which is `NULL` here and
/// `?` in MySQL, because the value is substituted before the statement is
/// parsed.
#[test]
fn describe_matches_execute_with_parameters() {
    let (engine, mut s) = setup();
    for (q, params) in [
        ("SELECT id, name FROM A WHERE id = ?", vec![Value::Int(1)]),
        (
            "SELECT id FROM A WHERE name = ? AND k > ?",
            vec![Value::text("one"), Value::Int(0)],
        ),
        ("SELECT id FROM A ORDER BY id LIMIT ?", vec![Value::Int(2)]),
    ] {
        let described = engine
            .describe(&s, q, params.len())
            .unwrap_or_else(|| panic!("described nothing: {q}"));
        let executed = match engine.execute(&mut s, q, &params) {
            Ok(Outcome::Rows(r)) => r.columns,
            other => panic!("{q} returned {other:?}"),
        };
        let f = |c: &[mysql_ultralight_engine::OutColumn]| {
            c.iter()
                .map(|c| format!("{}:{:?}", c.name, c.ty))
                .collect::<Vec<_>>()
        };
        assert_eq!(f(&described), f(&executed), "{q}");
    }

    // The one shape where the two legitimately differ, asserted rather than
    // left in prose so it cannot drift: the parameter *is* the column, so
    // there is nothing but the bound value to type it by, and the value is
    // NULL when describing. The name is what MySQL would call `?`.
    let described = engine.describe(&s, "SELECT ? AS lit", 1).unwrap();
    let Ok(Outcome::Rows(executed)) = engine.execute(&mut s, "SELECT ? AS lit", &[Value::Int(7)])
    else {
        panic!("SELECT ? AS lit did not return rows");
    };
    assert_eq!(described[0].name, "lit");
    assert_eq!(executed.columns[0].name, "lit");
    assert_ne!(described[0].ty, executed.columns[0].ty);

    // Unaliased, the column is named for the bound NULL rather than `?`.
    let bare = engine.describe(&s, "SELECT ?", 1).unwrap();
    assert_eq!(bare[0].name, "NULL");
}

/// A lateral `JSON_TABLE` over a table with no rows.
///
/// The document expression is correlated, so with no left row to resolve it
/// against the relation used to fail with 1054 — at execution, not only when
/// describing. An empty table is an ordinary thing for a test database to
/// hold, and the answer is no rows rather than an error.
#[test]
fn a_lateral_json_table_over_an_empty_table_has_columns() {
    let (engine, mut s) = setup();
    engine
        .execute(&mut s, "CREATE TABLE E (id INT, j JSON)", &[])
        .unwrap();

    for q in [
        "SELECT jt.n FROM E, JSON_TABLE(E.j, '$[*]' COLUMNS(n INT PATH '$.n')) jt",
        "SELECT E.id, jt.n FROM E LEFT JOIN JSON_TABLE(E.j, '$[*]' COLUMNS(n INT PATH '$.n')) jt ON 1 = 1",
    ] {
        let Ok(Outcome::Rows(r)) = engine.execute(&mut s, q, &[]) else {
            panic!("{q} did not return rows");
        };
        assert!(r.rows.is_empty(), "{q}");
        assert_eq!(r.columns.last().map(|c| c.name.clone()), Some("n".to_owned()), "{q}");

        // And the same columns without running it.
        let described = engine
            .describe(&s, q, 0)
            .unwrap_or_else(|| panic!("described nothing: {q}"));
        assert_eq!(
            described.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            r.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            "{q}"
        );
    }
}
