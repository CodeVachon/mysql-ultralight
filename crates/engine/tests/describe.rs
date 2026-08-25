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
        let described = engine.describe(&s, q, 0).unwrap();
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
            .unwrap()
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
    let described = engine.describe(&s, "SELECT ? AS lit", 1).unwrap().unwrap();
    let Ok(Outcome::Rows(executed)) = engine.execute(&mut s, "SELECT ? AS lit", &[Value::Int(7)])
    else {
        panic!("SELECT ? AS lit did not return rows");
    };
    assert_eq!(described[0].name, "lit");
    assert_eq!(executed.columns[0].name, "lit");
    assert_ne!(described[0].ty, executed.columns[0].ty);

    // Unaliased, the column is named for the bound NULL rather than `?`.
    let bare = engine.describe(&s, "SELECT ?", 1).unwrap().unwrap();
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
            .describe(&s, q, 0).unwrap()
            .unwrap_or_else(|| panic!("described nothing: {q}"));
        assert_eq!(
            described.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            r.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            "{q}"
        );
    }
}

/// Every refusal below is `PREPARE`d and then run against MySQL 8.3, and the
/// code, the SQLSTATE and the message are transcribed from what it said.
const REFUSED: &[(&str, u16, &str, &str)] = &[
    // The statement from #0058: no table or alias `nope` is in scope.
    (
        "SELECT nope.k FROM A",
        1054,
        "42S22",
        "Unknown column 'nope.k' in 'field list'",
    ),
    // Unqualified, and qualified by a table that *is* in scope. Both are the
    // same 1054, and the name is echoed back exactly as written.
    (
        "SELECT zz FROM A",
        1054,
        "42S22",
        "Unknown column 'zz' in 'field list'",
    ),
    (
        "SELECT A.zz FROM A",
        1054,
        "42S22",
        "Unknown column 'A.zz' in 'field list'",
    ),
    // A qualifier naming a real table the FROM clause did not mention. The
    // table exists; being in the database is not being in scope.
    (
        "SELECT B.label FROM A",
        1054,
        "42S22",
        "Unknown column 'B.label' in 'field list'",
    ),
    // An alias replaces the table name rather than adding to it.
    (
        "SELECT A.k FROM A x",
        1054,
        "42S22",
        "Unknown column 'A.k' in 'field list'",
    ),
    // Inside a call, and inside an aggregate — which takes the grouped path.
    (
        "SELECT UPPER(nope.k) FROM A",
        1054,
        "42S22",
        "Unknown column 'nope.k' in 'field list'",
    ),
    (
        "SELECT COUNT(nope.k) FROM A",
        1054,
        "42S22",
        "Unknown column 'nope.k' in 'field list'",
    ),
    // Resolution runs before the grouping rules: 8.3 answers 1054 here, not
    // the 1055 that ONLY_FULL_GROUP_BY would give the same statement.
    (
        "SELECT nope.k, COUNT(*) FROM A GROUP BY k",
        1054,
        "42S22",
        "Unknown column 'nope.k' in 'field list'",
    ),
    // WHERE, which MySQL names differently in the message.
    (
        "SELECT k FROM A WHERE nope.k = 1",
        1054,
        "42S22",
        "Unknown column 'nope.k' in 'where clause'",
    ),
    (
        "SELECT COUNT(*) FROM A WHERE nope.k = 1",
        1054,
        "42S22",
        "Unknown column 'nope.k' in 'where clause'",
    ),
    // An output alias is not in scope in WHERE, which is where MySQL differs
    // from the reading a lot of people expect.
    (
        "SELECT k + 1 AS x FROM A WHERE x > 1",
        1054,
        "42S22",
        "Unknown column 'x' in 'where clause'",
    ),
    // A CTE and a derived table are in scope under their own names, and a
    // column they do not select is still unknown.
    (
        "WITH c AS (SELECT id FROM A) SELECT c.zz FROM c",
        1054,
        "42S22",
        "Unknown column 'c.zz' in 'field list'",
    ),
    (
        "SELECT d.zz FROM (SELECT id FROM A) d",
        1054,
        "42S22",
        "Unknown column 'd.zz' in 'field list'",
    ),
    // A wildcard's qualifier is a *table* that is not in scope, so it is 1051
    // rather than 1054, and the engine used to answer both with a result set
    // holding no columns at all.
    (
        "SELECT nope.* FROM A",
        1051,
        "42S02",
        "Unknown table 'nope'",
    ),
    ("SELECT A.* FROM A x", 1051, "42S02", "Unknown table 'A'"),
    // Back-quoted, which is how a generated client writes every identifier.
    // The quotes belong to the spelling and not to the name: MySQL reports
    // `nope.k` and `nope` without them. Reading the qualifier off the
    // rendered text rather than off its identifiers put the quotes into the
    // comparison, where no table can match them — so the legal half of this
    // pair refused, and that half is in `a_name_in_scope_is_still_in_scope`.
    (
        "SELECT `nope`.`k` FROM A",
        1054,
        "42S22",
        "Unknown column 'nope.k' in 'field list'",
    ),
    (
        "SELECT `nope`.* FROM A",
        1051,
        "42S02",
        "Unknown table 'nope'",
    ),
];

/// A reference nothing in scope answers to is refused when the statement is
/// prepared, not when it is run.
///
/// MySQL raises these at PREPARE, and a client that prepares its statements up
/// front — which is what the binary protocol is for, and what Prisma does —
/// learns about the mistake there. This engine used to describe
/// `SELECT nope.k FROM A` as one column called `k` and fail only on execution,
/// because a name is resolved when a row is evaluated against it and
/// describing evaluates no rows.
#[test]
fn a_reference_nothing_answers_to_is_refused_at_prepare() {
    let (engine, s) = setup();
    for (sql, code, state, message) in REFUSED {
        let refused = engine
            .describe(&s, sql, 0)
            .expect_err(&format!("described happily: {sql}"));
        assert_eq!(
            (
                refused.code,
                refused.sql_state.as_str(),
                refused.message.as_str()
            ),
            (*code, *state, *message),
            "{sql}"
        );
    }
}

/// And the same statement fails the same way when it is executed.
///
/// Held against the prepare-time answer rather than asserted separately: the
/// bug this closes was the two disagreeing, so a test that let them drift
/// apart again would be no test at all.
#[test]
fn what_prepare_refuses_execution_refuses_identically() {
    let (engine, mut s) = setup();
    for (sql, code, state, message) in REFUSED {
        let ran = engine
            .execute(&mut s, sql, &[])
            .expect_err(&format!("ran happily: {sql}"));
        assert_eq!(
            (ran.code, ran.sql_state.as_str(), ran.message.as_str()),
            (*code, *state, *message),
            "{sql}"
        );
    }
}

/// An empty table does not make a bad reference legal.
///
/// The same gap as describing, from the other end: a name was resolved once
/// per row, so a table with no rows — the ordinary state of a test database
/// before its fixtures land — answered `SELECT nope.k FROM Empty` with an
/// empty result set instead of 1054. Every statement here has a `FROM` that
/// yields nothing, by being empty, by being filtered to nothing, and by a
/// `LIMIT 0`.
#[test]
fn an_empty_result_does_not_make_a_bad_reference_legal() {
    let (engine, mut s) = setup();
    engine
        .execute(&mut s, "CREATE TABLE Empty (id INT NOT NULL, k INT)", &[])
        .unwrap();

    for sql in [
        "SELECT nope.k FROM Empty",
        "SELECT k FROM Empty WHERE nope.k = 1",
        "SELECT COUNT(nope.k) FROM Empty",
        "SELECT nope.* FROM Empty",
        "SELECT nope.k FROM A WHERE 1 = 0",
        "SELECT nope.k FROM A LIMIT 0",
    ] {
        let ran = engine
            .execute(&mut s, sql, &[])
            .err()
            .unwrap_or_else(|| panic!("answered rather than refused: {sql}"));
        assert!(
            matches!(ran.code, 1054 | 1051),
            "{sql} gave {} {}",
            ran.code,
            ran.message
        );
    }
}

/// The shapes in which a name *is* in scope, which must keep working.
///
/// The risk in resolving a qualifier is refusing something legal, and every
/// entry here is a place a scope comes from somewhere other than the FROM
/// clause of the query the reference is written in: an enclosing query, a
/// CTE, a derived table, a join that merges two columns into one, an output
/// alias, the current database spelled out. Each is prepared and run, because
/// the two used to disagree.
#[test]
fn a_name_in_scope_is_still_in_scope() {
    let (engine, mut s) = setup();
    for sql in [
        // Correlated: `A` is in scope inside the subquery, one level down and
        // in three positions.
        "SELECT A.k, (SELECT COUNT(*) FROM B WHERE B.aid = A.id) n FROM A",
        "SELECT A.k FROM A WHERE EXISTS (SELECT 1 FROM B b WHERE b.aid = A.id)",
        "SELECT A.k FROM A WHERE A.id IN (SELECT b.aid FROM B b WHERE b.id = A.id)",
        "SELECT (SELECT MAX(b.aid) FROM B b WHERE b.id = A.id) m FROM A",
        // A CTE and a derived table, under their own names.
        "WITH c AS (SELECT id, k FROM A) SELECT c.k FROM c",
        "SELECT d.k FROM (SELECT id, k FROM A) d",
        // An alias, including one that shadows a real table name.
        "SELECT x.k FROM A x",
        "SELECT a1.k, a2.name FROM A a1 JOIN A a2 ON a1.k = a2.k",
        // USING merges the two `id` columns into one, so the qualified form
        // still resolves.
        "SELECT A.id FROM A JOIN B USING (id)",
        // An output alias is in scope in ORDER BY, unlike in WHERE.
        "SELECT k AS ali FROM A ORDER BY ali",
        // The current database written out in front of the table.
        "SELECT A.k FROM t.A",
        "SELECT t.A.k FROM A",
        // A wildcard qualified by something that is in scope.
        "SELECT A.* FROM A",
        "SELECT x.* FROM A x",
        // And the same shapes back-quoted. A qualifier's quotes are not part
        // of its name, and reading them as part of it refused every one of
        // these with 1051 or 1054.
        "SELECT `A`.`k` FROM A",
        "SELECT `A`.* FROM A",
        "SELECT `x`.* FROM A `x`",
        "SELECT `t`.`A`.* FROM A",
        // A bare keyword that parses as an identifier and is not a column.
        "SELECT TIMESTAMPDIFF(DAY, at, at) d FROM A",
        // A variable is not a column, in either spelling — and the second
        // parses as a compound identifier, which is the shape a qualified
        // column reference has.
        "SELECT @@GLOBAL.version, A.k FROM A",
        "SELECT A.k FROM A WHERE @@GLOBAL.version IS NOT NULL",
        "SELECT A.k FROM A WHERE @@version_comment IS NOT NULL",
    ] {
        let described = engine
            .describe(&s, sql, 0)
            .unwrap_or_else(|e| panic!("refused a legal statement: {sql} -> {e}"))
            .unwrap_or_else(|| panic!("described nothing: {sql}"));
        let Ok(Outcome::Rows(ran)) = engine.execute(&mut s, sql, &[]) else {
            panic!("did not return rows: {sql}");
        };
        assert_eq!(
            described.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            ran.columns
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>(),
            "{sql}"
        );
    }
}

/// A qualified `*` is read in the query that wrote it, and in nothing else.
///
/// Unlike a named column, which sees the enclosing query, a wildcard resolves
/// against its own FROM clause only: `1 IN (SELECT A.* FROM B)` is
/// `Unknown table 'A'` on 8.3 even with `A` in the outer FROM. The exception
/// is EXISTS, where MySQL throws the select list away before expanding any
/// `*` in it — so a qualifier that names nothing at all is accepted there, a
/// *named* column in the same position is still 1054, and a HAVING keeps the
/// list alive long enough for the `*` to be resolved after all.
///
/// Every expectation below is `PREPARE`d and run against 8.3.
#[test]
fn a_wildcard_qualifier_is_read_in_the_query_that_wrote_it() {
    let (engine, mut s) = setup();

    // EXISTS discards the list, so none of these resolve the qualifier —
    // through DISTINCT, an extra item, GROUP BY, a UNION, ORDER BY, a
    // negation, and one nested inside another subquery.
    for (sql, expected) in [
        ("SELECT COUNT(*) FROM A WHERE EXISTS (SELECT A.* FROM B)", 3),
        (
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT nope.* FROM B)",
            3,
        ),
        (
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT DISTINCT nope.* FROM B)",
            3,
        ),
        (
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT nope.*, B.id FROM B)",
            3,
        ),
        (
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT nope.* FROM B GROUP BY B.id)",
            3,
        ),
        (
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT nope.* FROM B UNION SELECT B.* FROM B)",
            3,
        ),
        (
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT nope.* FROM B ORDER BY B.id)",
            3,
        ),
        // One column, not none: what replaces the `*` has an arity, and a
        // UNION compares it. 8.3 accepts this and refuses the two-column
        // version below with 1222, which is only true if the first branch
        // became exactly one column.
        (
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT nope.* FROM B UNION SELECT B.id FROM B)",
            3,
        ),
        // The subquery matches nothing, so the answer turns on the rows and
        // not only on the statement being accepted.
        (
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT nope.* FROM B WHERE B.id = 99)",
            0,
        ),
        (
            "SELECT COUNT(*) FROM A WHERE NOT EXISTS (SELECT nope.* FROM B WHERE B.id = 99)",
            3,
        ),
    ] {
        let Ok(Outcome::Rows(ran)) = engine.execute(&mut s, sql, &[]) else {
            panic!("refused a statement MySQL answers: {sql}");
        };
        assert_eq!(
            ran.rows
                .first()
                .and_then(|r| r.first())
                .map(|v| v.as_text()),
            Some(expected.to_string()),
            "{sql}"
        );
        engine
            .describe(&s, sql, 0)
            .unwrap_or_else(|e| panic!("refused at prepare: {sql} -> {e}"));
    }

    let mismatched = engine
        .execute(
            &mut s,
            "SELECT COUNT(*) FROM A WHERE EXISTS \
             (SELECT nope.* FROM B UNION SELECT B.id, B.aid FROM B)",
            &[],
        )
        .expect_err("a one-column branch was compared against two and agreed");
    assert_eq!(mismatched.code, 1222);

    // A HAVING reads the select list, so the list is kept and the `*` in it
    // *is* resolved — the same statement without the HAVING is two entries
    // above and answers. 8.3 refuses this with 1051; this engine refuses it
    // one step earlier, with 1235, because a qualified `*` in a grouped query
    // is not implemented at all. Both refuse, which is what the guard is for;
    // the code will agree once that gap closes.
    let kept = engine
        .execute(
            &mut s,
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT nope.* FROM B HAVING COUNT(*) > 0)",
            &[],
        )
        .expect_err("a HAVING did not keep the select list alive");
    assert_eq!(kept.code, 1235);

    // A named column in that same position is resolved, and refused.
    let named = engine
        .execute(
            &mut s,
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT nope.k FROM B)",
            &[],
        )
        .expect_err("EXISTS swallowed a column reference too");
    assert_eq!(named.code, 1054);
    assert_eq!(named.message, "Unknown column 'nope.k' in 'field list'");

    // Everywhere the list survives, the qualifier is resolved against this
    // query's own FROM clause — where an outer table is not, and where the
    // engine used to hand back a result set with no columns in it.
    for sql in [
        "SELECT id FROM A WHERE 1 IN (SELECT A.* FROM B)",
        "SELECT id FROM A WHERE 1 IN (SELECT nope.* FROM B)",
        "SELECT id FROM A WHERE id = (SELECT A.* FROM B LIMIT 1)",
    ] {
        let refused = engine
            .execute(&mut s, sql, &[])
            .expect_err(&format!("answered with no columns: {sql}"));
        assert_eq!(refused.code, 1051, "{sql}");
        assert_eq!(refused.sql_state, "42S02", "{sql}");
    }
}
