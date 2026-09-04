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
/// column, that is visible: `SELECT ?` is described by the NULL it was bound
/// to and executes as whatever the client sent.
///
/// The *name* is not one of the things that may differ, and since #0109 it
/// does not: `?` at prepare and `?` at execute, which is what 8.3 answers
/// through a real prepared statement whatever is bound.
///
/// The declared *type* still differs from MySQL's, and is not this issue's:
/// measured through `COM_STMT_PREPARE`, 8.3 describes `SELECT ?` as
/// `VAR_STRING` of 65532 bytes and this engine as `MYSQL_TYPE_NULL` of nought,
/// because 8.3 types a parameter from the statement and this engine types it
/// from the value it was bound to. That is parameter type inference rather
/// than naming, and it is unfiled.
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
    // left in prose so it cannot drift: the parameter *is* the column, and
    // 8.3 re-describes it by the bound value's type at execute where the
    // prepare has no value and types it from the statement. Both halves are
    // pinned in `a_parameter_is_typed_from_the_statement_and_not_from_the_value`;
    // what this line holds is only that the two are allowed to differ, and
    // that they differ in the *type* and never in the name. #0228.
    let described = engine.describe(&s, "SELECT ? AS lit", 1).unwrap().unwrap();
    let Ok(Outcome::Rows(executed)) = engine.execute(&mut s, "SELECT ? AS lit", &[Value::Int(7)])
    else {
        panic!("SELECT ? AS lit did not return rows");
    };
    assert_eq!(described[0].name, "lit");
    assert_eq!(executed.columns[0].name, "lit");
    assert_ne!(described[0].ty, executed.columns[0].ty);

    // The *name* is not one of the things that may differ, and unaliased it is
    // the one thing left to compare: `?` at describe and `?` at execute, which
    // is what 8.3 answers through a real prepared statement whatever is bound.
    // #0109 — it used to be `NULL` here and the value there.
    let bare = engine.describe(&s, "SELECT ?", 1).unwrap().unwrap();
    let Ok(Outcome::Rows(ran)) = engine.execute(&mut s, "SELECT ?", &[Value::Int(7)]) else {
        panic!("SELECT ? did not return rows");
    };
    assert_eq!(bare[0].name, "?");
    assert_eq!(ran.columns[0].name, "?");
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
    // The database beside the qualifier, which was never compared against
    // anything: `A` is in scope and `nodb` is not the database it is in, so
    // 8.3 answers these two and this engine answered rows and every column.
    // The one that matters is not the misspelling but the statement written
    // against a *different* schema, which silently read this one.
    (
        "SELECT nodb.A.k FROM A",
        1054,
        "42S22",
        "Unknown column 'nodb.A.k' in 'field list'",
    ),
    (
        "SELECT nodb.A.* FROM A",
        1051,
        "42S02",
        "Unknown table 'nodb.A'",
    ),
    // Back-quoted, for the reason the pair above it is: the quotes belong to
    // the spelling, and the database part is read off the identifier like
    // every other part.
    (
        "SELECT `nodb`.`A`.`k` FROM A",
        1054,
        "42S22",
        "Unknown column 'nodb.A.k' in 'field list'",
    ),
    // A derived table is in no database, so a qualified wildcard over one is
    // 1051 whatever database it names — including the right one. The column
    // form of the same reference answers, and is in
    // `a_name_in_scope_is_still_in_scope`.
    (
        "SELECT t.d.* FROM (SELECT id FROM A) d",
        1051,
        "42S02",
        "Unknown table 't.d'",
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
        // The current database written out in front of the table, on the
        // reference as well as on the table, and through an alias — which
        // renames the table part and leaves the database alone.
        "SELECT A.k FROM t.A",
        "SELECT t.A.k FROM A",
        "SELECT t.A.k FROM t.A",
        "SELECT t.x.k FROM A x",
        "SELECT t.A.* FROM A",
        // A derived table and a CTE are in no database, so the qualifier on a
        // *column* is compared against nothing there and any database answers.
        // Measured on 8.3, where the wildcard form of the same reference is
        // 1051 — the two clauses read the absence in opposite directions.
        "SELECT nodb.d.k FROM (SELECT id, k FROM A) d",
        "SELECT t.d.k FROM (SELECT id, k FROM A) d",
        "WITH c AS (SELECT id, k FROM A) SELECT nodb.c.k FROM c",
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
    // above and answers. 1051, which is what 8.3 answers: the gap this used to
    // record closed with #0099, which moved the field list's resolution ahead
    // of the grouped path so that the select list is read before anything
    // else in the query body. It was 1235 here until then — a qualified `*` in
    // a grouped query is still not implemented, and the statement reached that
    // refusal before it reached this one.
    let kept = engine
        .execute(
            &mut s,
            "SELECT COUNT(*) FROM A WHERE EXISTS (SELECT nope.* FROM B HAVING COUNT(*) > 0)",
            &[],
        )
        .expect_err("a HAVING did not keep the select list alive");
    assert_eq!(kept.code, 1051);
    assert_eq!(kept.message, "Unknown table 'nope'");

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

/// The metadata a column is *described* with, as one line per column.
///
/// The two protocols encode by these numbers and a client reads them to decide
/// how to decode — and, for an ORM, how to type — the values that follow, so
/// they are asserted whole rather than one field at a time: a rule that fixes
/// the places and breaks the width is a rule that has to fail here.
///
/// Both ways round, for the reason this file exists: a shape derived from the
/// schema and a shape derived from the rows agree only by construction, and
/// `describe` never sees a row.
fn shapes(engine: &std::sync::Arc<Engine>, s: &mut Session, sql: &str) -> Vec<String> {
    use mysql_ultralight_protocol::column::flags;

    let render = |columns: &[mysql_ultralight_engine::OutColumn]| {
        columns
            .iter()
            .map(|c| {
                let def = c.to_wire();
                let named = [
                    (flags::NOT_NULL, "NOT_NULL"),
                    (flags::UNSIGNED, "UNSIGNED"),
                    (flags::BINARY, "BINARY"),
                    (flags::BLOB, "BLOB"),
                ]
                .iter()
                .filter(|(bit, _)| def.flags & bit != 0)
                .map(|(_, name)| *name)
                .collect::<Vec<_>>()
                .join("|");
                format!(
                    "{} {:?} len {} dec {} {}",
                    c.name, def.column_type, def.column_length, def.decimals, named
                )
            })
            .collect::<Vec<_>>()
    };

    let executed = match engine.execute(s, sql, &[]) {
        Ok(Outcome::Rows(r)) => r.columns,
        other => panic!("{sql} returned {other:?}"),
    };
    let described = engine
        .describe(s, sql, 0)
        .unwrap()
        .unwrap_or_else(|| panic!("described nothing: {sql}"));
    assert_eq!(render(&described), render(&executed), "{sql}");
    render(&executed)
}

/// #0111: what a number is described with, held against MySQL 8.3.
///
/// Every expectation below was measured with `COLINFO=1 scripts/ask.sh`
/// against `mysql:8.3` over the same table, and every computed one of them was
/// `len 20 dec 0` here before. A `DECIMAL(10,2)` described with no places is
/// what Prisma reads to decide a money column holds whole numbers.
#[test]
fn a_number_is_described_with_its_precision_and_scale() {
    let (engine, mut s) = setup();
    engine
        .execute(
            &mut s,
            "CREATE TABLE N (a DECIMAL(10,2), b DECIMAL(7,3), w DECIMAL(5,0), \
             u DECIMAL(8,3) UNSIGNED, big DECIMAL(65,30), small DECIMAL(3,3), \
             f FLOAT, fm FLOAT(10,4), d DOUBLE, i INT, at DATETIME(3), tm TIME(6))",
            &[],
        )
        .unwrap();
    engine
        .execute(
            &mut s,
            "INSERT INTO N VALUES (10.25, 2.125, 7, 3.500, 1.5, 0.125, 1.5, 1.5, 2.5, 4, \
             '2026-01-02 03:04:05.678', '03:04:05.678901')",
            &[],
        )
        .unwrap();

    // A declaration reports its own scale, and an approximate column with none
    // reports `NOT_FIXED_DEC` where a string reports 0 — the split between a
    // field and an item.
    assert_eq!(
        shapes(&engine, &mut s, "SELECT a, b, w, u, big, small FROM N"),
        [
            "a NewDecimal len 12 dec 2 ",
            "b NewDecimal len 9 dec 3 ",
            "w NewDecimal len 6 dec 0 ",
            "u NewDecimal len 9 dec 3 UNSIGNED",
            "big NewDecimal len 67 dec 30 ",
            "small NewDecimal len 5 dec 3 ",
        ]
    );
    assert_eq!(
        shapes(&engine, &mut s, "SELECT f, fm, d, i, at, tm FROM N"),
        [
            "f Float len 12 dec 31 ",
            "fm Float len 10 dec 4 ",
            "d Double len 22 dec 31 ",
            "i Long len 11 dec 0 ",
            "at DateTime len 23 dec 3 BINARY",
            "tm Time len 17 dec 6 BINARY",
        ]
    );

    // The arithmetic. `+` takes the wider integral part plus one for the carry
    // and the wider scale, `*` adds both precisions and both scales, `/` adds
    // `div_precision_increment` to the left scale, and `%` works on the two
    // *widths* — which is the only rule that gets 12 rather than 13 for
    // `a MOD b`.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT a + b AS s, a - b AS d, a * b AS p, a / b AS q, a MOD b AS r FROM N"
        ),
        [
            "s NewDecimal len 14 dec 3 BINARY",
            "d NewDecimal len 14 dec 3 BINARY",
            "p NewDecimal len 19 dec 5 BINARY",
            "q NewDecimal len 19 dec 6 BINARY",
            "r NewDecimal len 12 dec 3 BINARY",
        ]
    );
    // The aggregates: a total needs room for every row it adds up, an average
    // gains the places a quotient does, and the two that hand a value back
    // keep its shape.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT SUM(a) AS t, AVG(a) AS m, MIN(a) AS lo, MAX(a) AS hi FROM N"
        ),
        [
            "t NewDecimal len 34 dec 2 BINARY",
            "m NewDecimal len 16 dec 6 BINARY",
            "lo NewDecimal len 12 dec 2 BINARY",
            "hi NewDecimal len 12 dec 2 BINARY",
        ]
    );
    // Rounding gives a digit back where truncating does not, and a cast names
    // the shape outright.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT ROUND(a, 1) AS r1, ROUND(a) AS r0, TRUNCATE(a, 1) AS t1, \
             CAST(a AS DECIMAL(12,4)) AS c, ABS(b) AS ab, -a AS neg FROM N"
        ),
        [
            "r1 NewDecimal len 12 dec 1 BINARY",
            "r0 NewDecimal len 10 dec 0 BINARY",
            "t1 NewDecimal len 11 dec 1 BINARY",
            "c NewDecimal len 14 dec 4 BINARY",
            "ab NewDecimal len 9 dec 3 BINARY",
            "neg NewDecimal len 12 dec 2 BINARY",
        ]
    );
    // The widths that come from somewhere other than a pair of DECIMALs: a
    // scale of zero costs no column for the point, an integer operand carries
    // its display width less its sign, and addition does *not* clamp its
    // precision where multiplication does — 8.3 reports 68 here, past its own
    // maximum of 65.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT w + w AS ww, big + small AS bs, a + 1 AS one, a * 2 AS two, \
             i + 1.5 AS mix, i / 1.5 AS q FROM N"
        ),
        [
            "ww NewDecimal len 7 dec 0 BINARY",
            "bs NewDecimal len 68 dec 30 BINARY",
            "one NewDecimal len 13 dec 2 BINARY",
            "two NewDecimal len 13 dec 2 BINARY",
            "mix NewDecimal len 14 dec 1 BINARY",
            "q NewDecimal len 17 dec 4 BINARY",
        ]
    );
    // The conditional family picks one of its candidates rather than computing
    // a new number, so it takes the widest of each part and adds no carry:
    // 13 where the addition of the same two columns is 14.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT GREATEST(a, b) AS g, COALESCE(a, b) AS c, IF(1, a, b) AS f, \
             CASE WHEN 1 THEN a ELSE b END AS e FROM N"
        ),
        [
            "g NewDecimal len 13 dec 3 BINARY",
            "c NewDecimal len 13 dec 3 BINARY",
            "f NewDecimal len 13 dec 3 BINARY",
            "e NewDecimal len 13 dec 3 BINARY",
        ]
    );
    // An approximate expression has no fixed places at all unless a declared
    // scale puts some on it, and a temporal carries its fractional seconds
    // into the arithmetic over it — `at + 0` is exact and shows three.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT f + f AS ff, fm + fm AS mm, d / 2 AS dd, at + 0 AS a0 FROM N"
        ),
        [
            "ff Double len 23 dec 31 BINARY",
            "mm Double len 20 dec 4 BINARY",
            "dd Double len 23 dec 31 BINARY",
            "a0 NewDecimal len 20 dec 3 BINARY",
        ]
    );
}

/// #0111 again, from the other side: a view stores the shape of the expression
/// behind it rather than widening every computed decimal to a bare `decimal`.
///
/// Measured on 8.3, which declares the same: `CREATE TABLE t AS SELECT a + b`
/// over a `DECIMAL(10,2)` and a `DECIMAL(7,3)` is `decimal(12,3)` there, and
/// `a / b` is `decimal(17,6)`.
#[test]
fn a_view_over_a_computed_decimal_keeps_its_places() {
    let (engine, mut s) = setup();
    engine
        .execute(
            &mut s,
            "CREATE TABLE M (a DECIMAL(10,2), b DECIMAL(7,3))",
            &[],
        )
        .unwrap();
    engine
        .execute(&mut s, "INSERT INTO M VALUES (10.25, 2.125)", &[])
        .unwrap();
    engine
        .execute(
            &mut s,
            "CREATE VIEW MV AS SELECT a + b AS s, a / b AS q FROM M",
            &[],
        )
        .unwrap();

    let Ok(Outcome::Rows(declared)) = engine.execute(
        &mut s,
        "SELECT COLUMN_TYPE FROM information_schema.columns \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'MV' ORDER BY ORDINAL_POSITION",
        &[],
    ) else {
        panic!("the view has no columns");
    };
    let spellings: Vec<String> = declared.rows.iter().map(|r| r[0].as_text()).collect();
    assert_eq!(spellings, ["decimal(12,3)", "decimal(17,6)"]);
}

/// #0142: a string function's type code comes from how wide its answer can be.
///
/// Every code and every width below was measured with `COLINFO=1
/// scripts/ask.sh` against `mysql:8.3`, and every one of these was a
/// `VAR_STRING` of 4096 here before.
///
/// The widths are MySQL's `Item::max_length` in bytes, which for a blob-coded
/// item is four times the arithmetic's own answer — the connection character
/// set's bytes per character, applied a second time. That is measured too:
/// `UPPER(tx)` over a utf8mb4 `TEXT` is 1048560 on 8.3 and the arithmetic
/// produces 262140.
#[test]
fn a_string_functions_type_widens_with_its_argument() {
    let (engine, mut s) = setup();
    engine
        .execute(
            &mut s,
            "CREATE TABLE W (c CHAR(10), v VARCHAR(20), tt TINYTEXT, tx TEXT, \
             mt MEDIUMTEXT, lt LONGTEXT, tb TINYBLOB, bl BLOB, mb MEDIUMBLOB, \
             js JSON, en ENUM('one','two'))",
            &[],
        )
        .unwrap();
    engine
        .execute(
            &mut s,
            "INSERT INTO W VALUES ('a','b','c','d','e','f','g','h','i','{\"a\": 1}','one')",
            &[],
        )
        .unwrap();

    // The threshold, both sides of it. A `TINYTEXT` still fits a VARCHAR and a
    // `TEXT` does not, which is the whole of the rule.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT UPPER(c) AS a, UPPER(v) AS b, UPPER(tt) AS d, UPPER(en) AS e FROM W"
        ),
        [
            "a VarString len 40 dec 31 ",
            "b VarString len 80 dec 31 ",
            "d VarString len 1020 dec 31 ",
            // An ENUM is as wide as its widest member, so this is three
            // characters and not the whole declaration.
            "e VarString len 12 dec 31 ",
        ]
    );
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT UPPER(tx) AS a, UPPER(mt) AS b, UPPER(lt) AS c FROM W"
        ),
        [
            "a MediumBlob len 1048560 dec 31 ",
            "b LongBlob len 268435440 dec 31 ",
            "c LongBlob len 4294967295 dec 31 ",
        ]
    );
    // `HEX` doubles the *bytes*, not the characters, which is what puts a
    // `BLOB` over the line and leaves a `TINYBLOB` under it.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT HEX(tb) AS a, HEX(c) AS b, HEX(bl) AS d, HEX(mb) AS e, HEX(js) AS f FROM W"
        ),
        [
            "a VarString len 2040 dec 31 ",
            "b VarString len 320 dec 31 ",
            "d MediumBlob len 2097120 dec 31 ",
            "e LongBlob len 536870880 dec 31 ",
            "f LongBlob len 4294967295 dec 31 ",
        ]
    );
    // Joined widths add up, so two columns that each fit alone need not fit
    // together.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT CONCAT(c, c) AS a, CONCAT(c, 'x') AS b, CONCAT(tx, tx) AS d, \
             CONCAT(v, tx) AS e FROM W"
        ),
        [
            "a VarString len 80 dec 31 ",
            "b VarString len 44 dec 31 ",
            "d MediumBlob len 2097120 dec 31 ",
            "e MediumBlob len 1048880 dec 31 ",
        ]
    );
    // A count asked for in the statement is the whole answer, however wide the
    // column behind it; with no length, a substring is what is left after its
    // start.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT SUBSTRING(tx, 1, 3) AS a, LEFT(tx, 2) AS b, LPAD(tx, 5, 'x') AS d, \
             SUBSTRING(tx, 2) AS e, TRIM(tx) AS f FROM W"
        ),
        [
            "a VarString len 12 dec 31 ",
            "b VarString len 8 dec 31 ",
            "d VarString len 20 dec 31 ",
            "e MediumBlob len 1048544 dec 31 ",
            "f MediumBlob len 1048560 dec 31 ",
        ]
    );
}

/// The corners of #0111 and #0142 that a first pass got wrong, each measured
/// against 8.3 after the fact.
///
/// They are together because they are one lesson: a width rule derived from
/// the shape of the code rather than from a measurement is a rule that looks
/// right. `MID` was sized from its *position*, so the same slice was eight
/// bytes under one spelling and twenty under the other; `UNHEX` and
/// `FROM_BASE64` answer bytes and were multiplied by the character set anyway,
/// which pushed `UNHEX(bl)` over a `BLOB` into a MEDIUM_BLOB where 8.3 says
/// VAR_STRING; and `REPLACE` was assumed not to grow.
#[test]
fn the_widths_that_are_only_right_because_they_were_measured() {
    let (engine, mut s) = setup();
    engine
        .execute(
            &mut s,
            "CREATE TABLE Msr (a DECIMAL(10,2), d DECIMAL(9,9), e DECIMAL(4,4), dd DOUBLE, \
             c CHAR(10), v VARCHAR(20), tx TEXT, bl BLOB, tb TINYBLOB)",
            &[],
        )
        .unwrap();
    engine
        .execute(
            &mut s,
            "INSERT INTO Msr VALUES (10.25, 0.123456789, 0.1234, 1.5, 'ab', 'cd', 'ef', 'gh', 'ij')",
            &[],
        )
        .unwrap();

    // A number has at least one digit. Giving up all nine places of a
    // `DECIMAL(9,9)` leaves none by the arithmetic — nine is a whole word, so
    // no digit is handed back for the carry — and 8.3 declares `decimal(1,0)`.
    // `DECIMAL(4,4)` is the pair that separates the two spellings, where
    // `ROUND` reaches one on its own.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT ROUND(d, 0) AS r9, TRUNCATE(d, 0) AS t9, ROUND(e, 0) AS r4, \
             TRUNCATE(e, 0) AS t4 FROM Msr"
        ),
        [
            "r9 NewDecimal len 2 dec 0 BINARY",
            "t9 NewDecimal len 2 dec 0 BINARY",
            "r4 NewDecimal len 2 dec 0 BINARY",
            "t4 NewDecimal len 2 dec 0 BINARY",
        ]
    );
    // `NOT_FIXED_DEC` is not a scale, and reading it as one gave this subject
    // 21 digits and 31 places — a shape no number has, and a subtraction that
    // went under zero. Both of these panicked in a debug build before
    // `exact_precision` refused the pair.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT ROUND((SELECT AVG(dd) FROM Msr), 0) AS rs, \
             TRUNCATE((SELECT AVG(dd) FROM Msr), 0) AS ts, (SELECT AVG(dd) FROM Msr) AS sq FROM Msr"
        ),
        [
            "rs Double len 23 dec 31 BINARY",
            "ts Double len 23 dec 31 BINARY",
            "sq Double len 23 dec 31 BINARY",
        ]
    );
    // `MID` is `SUBSTRING`: its second argument is where to start, not how
    // much to take. Asserted beside the spelling it is a synonym of, because
    // the bug was that the two disagreed.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT MID(tx, 2, 5) AS m1, MID(tx, 2) AS m2, SUBSTRING(tx, 2, 5) AS m3, \
             SUBSTRING(tx, 2) AS m4, RIGHT(tx, 3) AS r3 FROM Msr"
        ),
        [
            "m1 VarString len 20 dec 31 ",
            "m2 MediumBlob len 1048544 dec 31 ",
            "m3 VarString len 20 dec 31 ",
            "m4 MediumBlob len 1048544 dec 31 ",
            "r3 VarString len 12 dec 31 ",
        ]
    );
    // A replacement longer than what it replaces widens the answer, by the
    // rule in `string_chars` — measured over thirteen shapes and not derived.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT REPLACE(c, 'a', 'zzz') AS rp, REPLACE(c, 'a', 'z') AS rp2, \
             REPLACE(tx, 'a', 'zz') AS rp3, REVERSE(c) AS rv, CONCAT_WS(',', c, v) AS cw FROM Msr"
        ),
        [
            "rp VarString len 80 dec 31 ",
            "rp2 VarString len 40 dec 31 ",
            "rp3 MediumBlob len 1048560 dec 31 ",
            "rv VarString len 40 dec 31 ",
            "cw VarString len 124 dec 31 ",
        ]
    );
    // The two that answer *bytes*. Their widths are not multiplied by the
    // character set, and the difference decides a type code: `UNHEX(bl)` is
    // 32768 bytes, which still fits a VARCHAR, where four times it would not.
    // `TO_BASE64` beside them answers characters and is multiplied.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT UNHEX(c) AS u1, UNHEX(bl) AS u2, UNHEX(tx) AS u3, TO_BASE64(c) AS b1, \
             TO_BASE64(bl) AS b2, FROM_BASE64(c) AS f1, FROM_BASE64(bl) AS f2 FROM Msr"
        ),
        [
            "u1 VarString len 20 dec 31 BINARY",
            "u2 VarString len 32768 dec 31 BINARY",
            "u3 MediumBlob len 131070 dec 31 BINARY",
            "b1 VarString len 224 dec 31 ",
            "b2 MediumBlob len 1416464 dec 31 ",
            "f1 VarString len 30 dec 31 BINARY",
            "f2 VarString len 49151 dec 31 BINARY",
        ]
    );
    // And the same four over a `TINYBLOB`, which is where base64's line
    // wrapping shows: 1376 rather than 1372.
    assert_eq!(
        shapes(
            &engine,
            &mut s,
            "SELECT HEX(tb) AS h1, TO_BASE64(tb) AS h2, FROM_BASE64(tb) AS h3, \
             UNHEX(tb) AS h4 FROM Msr"
        ),
        [
            "h1 VarString len 2040 dec 31 ",
            "h2 VarString len 1376 dec 31 ",
            "h3 VarString len 191 dec 31 BINARY",
            "h4 VarString len 128 dec 31 BINARY",
        ]
    );
}

/// A chain of operators whose shape is not modelled is walked once per node,
/// not once per path through it.
///
/// The width walk asks each operand for its shape and, where that comes back
/// unknown, for its scale alone. Reading the two from separate walks made the
/// cost double per operator: twenty `+ 0` terms over a `DATETIME(3)` — whose
/// packed width is deliberately not modelled, so every node takes the fallback
/// — took 2.3 seconds to describe, and twenty-two took eight. A client can
/// send that statement.
///
/// The bound is loose on purpose. It is not a benchmark: it fails at four
/// orders of magnitude, where the exponential version needed *seconds* and
/// anything linear or quadratic needs single-digit milliseconds.
#[test]
fn a_chain_of_unmodelled_operators_is_not_walked_exponentially() {
    let (engine, mut s) = setup();
    engine
        .execute(&mut s, "CREATE TABLE D (at DATETIME(3))", &[])
        .unwrap();
    engine
        .execute(
            &mut s,
            "INSERT INTO D VALUES ('2026-01-02 03:04:05.678')",
            &[],
        )
        .unwrap();

    let sql = format!("SELECT at{} FROM D", " + 0".repeat(22));
    let started = std::time::Instant::now();
    let described = engine.describe(&s, &sql, 0).unwrap().unwrap();
    let elapsed = started.elapsed();

    // The shape is still the right one: a `DATETIME(3)` computes as an exact
    // number carrying its three fractional places, however many times zero is
    // added to it.
    assert_eq!(described.len(), 1);
    assert_eq!(described[0].to_wire().decimals, 3);
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "describing 22 terms took {elapsed:?}"
    );
}

/// A cast to a scale wider than its precision is described without crashing.
///
/// `DECIMAL(3,10)` is not a type — both servers answer 1427 when the statement
/// runs — but a `COM_STMT_PREPARE` describes its columns before anything
/// validates the cast, and the shape walk was handed 3 digits and 10 places.
/// Every rule there subtracts one from the other, so it went under zero: a
/// panic in a debug build and a wrapped width in a release one, from a string
/// the client chose.
///
/// The answer is a shape rather than a refusal, because describing is not the
/// place that refuses; what matters is that it comes back at all.
#[test]
fn a_cast_to_an_impossible_shape_is_described_rather_than_crashing() {
    let (engine, mut s) = setup();
    engine
        .execute(&mut s, "CREATE TABLE Imp (a DECIMAL(10,2))", &[])
        .unwrap();

    for sql in [
        "SELECT ROUND(CAST(a AS DECIMAL(3,10)), 0) FROM Imp",
        "SELECT TRUNCATE(CAST(a AS DECIMAL(3,10)), 0) FROM Imp",
        "SELECT CAST(a AS DECIMAL(3,10)) + 1 FROM Imp",
    ] {
        let described = engine
            .describe(&s, sql, 0)
            .unwrap()
            .unwrap_or_else(|| panic!("described nothing: {sql}"));
        assert_eq!(described.len(), 1, "{sql}");
    }
}

/// Every clause is resolved at prepare, not when the first row reaches it.
///
/// #0100, the remainder of #0058. A `describe` is a probe over no rows, and
/// this engine settled a GROUP BY, a join's ON and an OVER spec's two clauses
/// by *evaluating* them — once per row. With no row there was nothing to
/// evaluate, so `COM_STMT_PREPARE` came back declaring columns for statements
/// MySQL refuses outright, and a client that prepares up front was told they
/// were fine.
///
/// Measured on 8.3 through the `mysql` crate's `COM_STMT_PREPARE`, which the
/// CLI cannot reach because it never prepares: every statement below is
/// refused there at prepare, with the code and the clause name asserted here.
///
/// The tables are empty on purpose, so the refusal cannot be coming from a
/// row. The same statements over a table with rows in it are `sql.rs`'s
/// `a_1054_names_the_clause_the_reference_was_written_in`, which is where
/// these names were first pinned.
#[test]
fn a_clause_is_refused_at_prepare_rather_than_at_the_first_row() {
    let (engine, mut s) = setup();
    for ddl in [
        "CREATE TABLE Cl (id INT NOT NULL, k INT NULL, s VARCHAR(20) NULL, PRIMARY KEY (id))",
        "CREATE TABLE Cr (id INT NOT NULL, k INT NULL, PRIMARY KEY (id))",
    ] {
        engine.execute(&mut s, ddl, &[]).unwrap();
    }

    for (sql, code, message) in [
        // The two #0058 already settled, kept beside the rest so a change that
        // undoes the mechanism shows up as more than one line.
        (
            "SELECT zz.q FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT k FROM Cl WHERE zz.q = 1",
            1054,
            "Unknown column 'zz.q' in 'where clause'",
        ),
        // The four clauses this closes.
        (
            "SELECT k FROM Cl GROUP BY zz.q",
            1054,
            "Unknown column 'zz.q' in 'group statement'",
        ),
        (
            "SELECT Cl.k FROM Cl JOIN Cr ON zz.q = Cr.id",
            1054,
            "Unknown column 'zz.q' in 'on clause'",
        ),
        (
            "SELECT Cl.k FROM Cl LEFT JOIN Cr ON zz.q = Cr.id",
            1054,
            "Unknown column 'zz.q' in 'on clause'",
        ),
        (
            "SELECT SUM(k) OVER (PARTITION BY zz.q) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'window partition by'",
        ),
        (
            "SELECT SUM(k) OVER (ORDER BY zz.q) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'window order by'",
        ),
        // An OVER spec does not see the select list's aliases where the
        // query's own ORDER BY does, measured both ways on 8.3 — which is why
        // the scope here is the relation's and not the projection's.
        (
            "SELECT k AS a, SUM(id) OVER (PARTITION BY a) FROM Cl",
            1054,
            "Unknown column 'a' in 'window partition by'",
        ),
        // What a clause carries besides its names travels with it, and 8.3
        // settles all of it at prepare too.
        (
            "SELECT SUM(k) OVER (PARTITION BY s LIKE 'a' ESCAPE '!!') FROM Cl",
            1210,
            "Incorrect arguments to ESCAPE",
        ),
        (
            "SELECT SUM(k) OVER (ORDER BY SIGN()) FROM Cl",
            1582,
            "Incorrect parameter count in the call to native function 'SIGN'",
        ),
        (
            "SELECT Cl.k FROM Cl JOIN Cr ON Cl.s LIKE 'a' ESCAPE '!!'",
            1210,
            "Incorrect arguments to ESCAPE",
        ),
    ] {
        let Err(error) = engine.describe(&s, sql, 0) else {
            panic!("described rather than refusing: {sql}");
        };
        assert_eq!(error.code, code, "{sql}");
        assert_eq!(error.message, message, "{sql}");
    }

    // And the shapes that are legal over the same empty tables still describe,
    // which is the direction that matters most: refusing something MySQL
    // answers is worse than the gap this closes.
    for sql in [
        "SELECT k AS a FROM Cl ORDER BY a",
        "SELECT k FROM Cl GROUP BY k",
        "SELECT Cl.k FROM Cl JOIN Cr ON Cl.id = Cr.id",
        "SELECT SUM(k) OVER (PARTITION BY k ORDER BY id) FROM Cl",
        "SELECT k FROM Cl WHERE id IN (SELECT id FROM Cr)",
    ] {
        engine
            .describe(&s, sql, 0)
            .unwrap_or_else(|e| panic!("refused a legal statement: {sql} -> {e:?}"))
            .unwrap_or_else(|| panic!("described nothing: {sql}"));
    }

    // A GROUP BY reads the select list's output names beside the relation's
    // columns — `SELECT k AS a FROM Cl GROUP BY a` is a legal query on 8.3 —
    // so the check above must not refuse one. Asserted apart from the list
    // because this engine describes *nothing* for it rather than the column
    // 8.3 declares: the ONLY_FULL_GROUP_BY check behind it does not see the
    // alias as covering the item, which is its own divergence and not this
    // one. What matters here is only that resolving ahead of the first row did
    // not turn a query MySQL answers into a refusal.
    assert!(
        engine
            .describe(&s, "SELECT k AS a FROM Cl GROUP BY a", 0)
            .is_ok(),
        "refused a GROUP BY that names a select alias"
    );
}

/// #0227: a subquery's interior is resolved at prepare, like every other clause.
///
/// The clause #0100 left open. `subquery_column` already ran the inner query
/// as a probe to type `(SELECT ...)`, and threw away whatever it raised with
/// `.ok()`, because a walker that types an expression has nowhere to report an
/// error — so a name nothing answers to inside a subquery described a column
/// happily and failed only once a row reached it, which over an empty outer
/// table is never.
///
/// Every expectation below was measured on `mysql:8.3` through the `mysql`
/// crate's `conn.prep` with the statement cache disabled — the CLI does not
/// prepare, and the crate refreshes a cached statement's columns from the
/// *execute* response, so a repeated statement hands back stale prepare
/// metadata. Cross-checked against the server-side `PREPARE p FROM '...'`
/// spelling, which agrees.
///
/// `Cl` is empty throughout, which is the whole point: 8.3 refuses each of
/// these before any row exists, and so did none of them here.
#[test]
fn a_subquerys_interior_is_resolved_at_prepare() {
    let (engine, mut s) = setup();
    for ddl in [
        "CREATE TABLE Cl (id INT NOT NULL, k INT NULL, s VARCHAR(20) NULL, PRIMARY KEY (id))",
        "CREATE TABLE Cr (q INT NULL, s VARCHAR(20) NULL)",
    ] {
        engine.execute(&mut s, ddl, &[]).unwrap();
    }

    for (sql, code, message) in [
        // A scalar subquery, which is the shape the issue was filed on.
        (
            "SELECT (SELECT zz.q FROM Cr) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        // An IN subquery and an EXISTS, which `subquery_column` never even
        // looked at: nothing types them, so nothing ran them.
        (
            "SELECT id FROM Cl WHERE id IN (SELECT zz.q FROM Cr)",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT id FROM Cl WHERE EXISTS (SELECT zz.q FROM Cr)",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        // `= ANY` and `> ALL` carry their subquery in a variant
        // `subexpressions` does not name, so the walk has to name it itself.
        (
            "SELECT id FROM Cl WHERE id = ANY (SELECT zz.q FROM Cr)",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT id FROM Cl WHERE id > ALL (SELECT zz.q FROM Cr)",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        // The clause named in the 1054 is the *inner* one, not the outer one
        // the subquery sits in. That falls out of running the inner query
        // rather than being decided here, and each of these was measured:
        (
            "SELECT (SELECT q FROM Cr WHERE zz.q = 1) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'where clause'",
        ),
        (
            "SELECT (SELECT q FROM Cr GROUP BY zz.q) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'group statement'",
        ),
        (
            "SELECT (SELECT q FROM Cr ORDER BY zz.q) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'order clause'",
        ),
        (
            "SELECT (SELECT q FROM Cr HAVING zz.q = 1) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'having clause'",
        ),
        (
            "SELECT (SELECT q FROM Cr JOIN Cl ON Cl.id = zz.q) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'on clause'",
        ),
        // A subquery in the outer query's other clauses, which reach the same
        // walk through a different caller.
        (
            "SELECT 1 FROM Cl WHERE 1 = 0 AND (SELECT zz.q FROM Cr) = 1",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT id FROM Cl GROUP BY (SELECT zz.q FROM Cr)",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT id FROM Cl ORDER BY (SELECT zz.q FROM Cr)",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT id FROM Cl HAVING (SELECT zz.q FROM Cr) = 1",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT Cl.id FROM Cl JOIN Cr ON Cl.id = (SELECT zz.q FROM Cr)",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        // Buried under a call, a CASE and a coalesce, where the old walk
        // stopped at the subquery node.
        (
            "SELECT CONCAT((SELECT zz.q FROM Cr)) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT CASE WHEN 0 THEN (SELECT zz.q FROM Cr) ELSE 1 END FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT MAX((SELECT zz.q FROM Cr)) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        // Two levels down, which only works because the probe of the outer
        // subquery resolves the inner one the same way.
        (
            "SELECT (SELECT (SELECT zz.q FROM Cr) FROM Cr) FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        // A correlated reference the *enclosing* query does not carry is a
        // 1054 as much as a made-up qualifier is: `Cl` is not in scope with
        // `Cr` alone in the outer FROM.
        (
            "SELECT (SELECT Cl.id FROM Cr) FROM Cr",
            1054,
            "Unknown column 'Cl.id' in 'field list'",
        ),
        // What a subquery's interior carries besides its names, settled the
        // same way and in the same walk.
        (
            "SELECT (SELECT s LIKE 'a' ESCAPE '!!' FROM Cr) FROM Cl",
            1210,
            "Incorrect arguments to ESCAPE",
        ),
        (
            "SELECT id FROM Cl WHERE id IN (SELECT q FROM Cr WHERE s LIKE 'a' ESCAPE '!!')",
            1210,
            "Incorrect arguments to ESCAPE",
        ),
        (
            "SELECT id FROM Cl WHERE EXISTS (SELECT q FROM Cr WHERE s LIKE 'a' ESCAPE '!!')",
            1210,
            "Incorrect arguments to ESCAPE",
        ),
        (
            "SELECT (SELECT SIGN() FROM Cr) FROM Cl",
            1582,
            "Incorrect parameter count in the call to native function 'SIGN'",
        ),
        (
            "SELECT id FROM Cl WHERE EXISTS (SELECT SIGN() FROM Cr)",
            1582,
            "Incorrect parameter count in the call to native function 'SIGN'",
        ),
        // A qualified `*` no FROM clause provides is 1051 inside a scalar
        // subquery and inside an IN — and *not* inside an EXISTS, which is
        // the legal shape asserted below.
        (
            "SELECT (SELECT Cl.* FROM Cr) FROM Cl",
            1051,
            "Unknown table 'Cl'",
        ),
        // Names before arguments, inside the subquery, positionally: 8.3
        // resolves an item left to right and answers whichever fault comes
        // first, so these two orders give two different codes.
        (
            "SELECT (SELECT zz.q FROM Cr WHERE s LIKE 'a' ESCAPE '!!') FROM Cl",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT (SELECT s LIKE 'a' ESCAPE '!!' FROM Cr WHERE zz.q = 1) FROM Cl",
            1210,
            "Incorrect arguments to ESCAPE",
        ),
        // And positionally *across* the items of the enclosing list, which is
        // what merging the subquery probe into the escape walk buys: a bad
        // subquery in the first item beats a bad escape in the second, and the
        // reverse order gives the reverse answer. Both measured on 8.3.
        (
            "SELECT (SELECT zz.q FROM Cr), s LIKE 'a' ESCAPE '!!' FROM Cr",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
        (
            "SELECT s LIKE 'a' ESCAPE '!!', (SELECT zz.q FROM Cr) FROM Cr",
            1210,
            "Incorrect arguments to ESCAPE",
        ),
        // `= ANY` and `> ALL` carry their left operand where
        // `subexpressions` does not look either, so the argument count of a
        // call written there has to be found by the same named arms. 8.3
        // raises 1582 in its parser, so it beats the subquery's own fault.
        (
            "SELECT id FROM Cl WHERE SIGN() = ANY (SELECT zz.q FROM Cr)",
            1582,
            "Incorrect parameter count in the call to native function 'SIGN'",
        ),
        // Still refused where the name is in no scope at all, which is what
        // stops the correlated ON below from being a way to accept anything.
        (
            "SELECT k FROM Cl WHERE EXISTS \
             (SELECT 1 FROM Cr JOIN Cr c3 ON c3.q = (SELECT zz.q FROM Cr))",
            1054,
            "Unknown column 'zz.q' in 'field list'",
        ),
    ] {
        let Err(error) = engine.describe(&s, sql, 0) else {
            panic!("described rather than refusing: {sql}");
        };
        assert_eq!(
            (error.code, error.message.as_str()),
            (code, message),
            "{sql}"
        );

        // The same statement, executed rather than prepared, over the same
        // empty tables. The invariant this file exists for: a refusal that
        // only happens at one of the two is the bug, not the fix.
        let Err(ran) = engine.execute(&mut s, sql, &[]) else {
            panic!("ran rather than refusing: {sql}");
        };
        assert_eq!((ran.code, ran.message.as_str()), (code, message), "{sql}");
    }

    // The direction that matters more: none of these may be refused. Every one
    // of them prepares on 8.3, and the correlated ones are exactly the shape a
    // walk that resolved a subquery in the wrong scope would break.
    for sql in [
        "SELECT (SELECT q FROM Cr) FROM Cl",
        "SELECT (SELECT Cl.id FROM Cr) FROM Cl",
        "SELECT (SELECT id FROM Cr) FROM Cl",
        "SELECT (SELECT MAX(q) FROM Cr WHERE Cr.q = Cl.id) FROM Cl",
        "SELECT id FROM Cl WHERE id IN (SELECT q FROM Cr)",
        "SELECT id FROM Cl WHERE EXISTS (SELECT 1 FROM Cr WHERE Cr.q = Cl.id)",
        "SELECT id FROM Cl c1 WHERE EXISTS (SELECT 1 FROM Cl c2 WHERE c2.id = c1.id)",
        // EXISTS throws its select list away before expanding any `*` in it,
        // so a qualifier nothing provides is legal there where it is 1051 two
        // lines up. Probing the subquery as written would have refused this.
        "SELECT id FROM Cl WHERE EXISTS (SELECT Cl.* FROM Cr)",
        "SELECT id FROM Cl WHERE EXISTS (SELECT nope.* FROM Cr)",
        // More than one column is legal under EXISTS too, where a scalar
        // subquery is 1241.
        "SELECT id FROM Cl WHERE EXISTS (SELECT q, s FROM Cr)",
        // A subquery written in a join's ON correlates outwards exactly as a
        // bare name does, so the probe has to read the enclosing query's
        // columns and not only the join's — the same scope `unresolved_column`
        // was already given, and the one a probe against `refs` alone turned
        // into a 1054 the moment either inner side was empty.
        "SELECT k FROM Cl WHERE EXISTS (SELECT 1 FROM Cr JOIN Cr c3 ON c3.q = (SELECT Cl.id))",
        "SELECT k FROM Cl WHERE EXISTS (SELECT 1 FROM Cr JOIN Cr c3 ON c3.q = Cl.id)",
        "SELECT Cl.id FROM Cl, LATERAL (SELECT q FROM Cr) d",
    ] {
        engine
            .describe(&s, sql, 0)
            .unwrap_or_else(|e| panic!("refused a legal statement: {sql} -> {e:?}"))
            .unwrap_or_else(|| panic!("described nothing: {sql}"));
        engine
            .execute(&mut s, sql, &[])
            .unwrap_or_else(|e| panic!("refused a legal statement at execute: {sql} -> {e:?}"));
    }
}

/// #0228: a parameter is typed from the statement, not from the value bound.
///
/// `describe` binds every parameter to NULL, because a statement's shape does
/// not depend on its arguments — and this engine then typed the column by that
/// NULL, so `SELECT ?` was `MYSQL_TYPE_NULL len 0`, the code for a column that
/// holds nothing at all. A client reads the describe response to decide how to
/// decode every value that follows, and Prisma prepares every statement.
///
/// Measured on `mysql:8.3` through the `mysql` crate's `conn.prep` with the
/// statement cache disabled, reading the column definitions of
/// `COM_STMT_PREPARE`. The differential harness cannot reach any of this:
/// `Answer::from_binary` prepares but binds nothing, so a parameterised case
/// fails client-side before any metadata is compared. That is why the coverage
/// is here.
///
/// The rule, derived rather than tabulated: a parameter has no type of its
/// own and takes the one its *position* gives it. With nothing to take — a
/// bare `?`, a string beside it, a string function around it — it is a
/// VAR_STRING as wide as `Item_param` is, 65532 bytes with `NOT_FIXED_DEC`
/// places. In arithmetic it takes the other operand's: a whole number beside
/// it makes it a LONGLONG 21 wide, an exact fraction makes it
/// `DECIMAL(65, 30)`, an approximate one makes it a DOUBLE, and another
/// parameter — nothing on either side — makes both DOUBLE.
#[test]
fn a_parameter_is_typed_from_the_statement_and_not_from_the_value() {
    let (engine, mut s) = setup();
    engine
        .execute(
            &mut s,
            "CREATE TABLE P (i INT NOT NULL, d DECIMAL(10,2) NULL, f FLOAT NULL, \
             s VARCHAR(10) NULL, dt DATETIME(3) NULL, dd DATE NULL)",
            &[],
        )
        .unwrap();

    let described = |sql: &str, parameters: usize| {
        let columns = engine
            .describe(&s, sql, parameters)
            .unwrap_or_else(|e| panic!("refused: {sql} -> {e:?}"))
            .unwrap_or_else(|| panic!("described nothing: {sql}"));
        columns
            .iter()
            .map(|c| {
                let def = c.to_wire();
                format!(
                    "{} {:?} len {} dec {}",
                    c.name, def.column_type, def.column_length, def.decimals
                )
            })
            .collect::<Vec<_>>()
    };

    for (sql, parameters, expected) in [
        // Nothing types it: a string, its own width, and no fixed places.
        ("SELECT ?", 1, "? VarString len 65532 dec 31"),
        ("SELECT ? AS a", 1, "a VarString len 65532 dec 31"),
        // Through the wrappers MySQL's parser discards.
        ("SELECT (?)", 1, "? VarString len 65532 dec 31"),
        ("SELECT +?", 1, "? VarString len 65532 dec 31"),
        // A whole number beside it makes it one, and the column is a digit
        // wider than the parameter for the carry.
        ("SELECT ?+1", 1, "?+1 LongLong len 22 dec 0"),
        ("SELECT 1+?", 1, "1+? LongLong len 22 dec 0"),
        ("SELECT ?*2", 1, "?*2 LongLong len 22 dec 0"),
        ("SELECT ? DIV 2", 1, "? DIV 2 LongLong len 21 dec 0"),
        ("SELECT ? % 2", 1, "? % 2 LongLong len 21 dec 0"),
        // An exact fraction makes it the widest decimal MySQL has.
        ("SELECT ?+1.5", 1, "?+1.5 NewDecimal len 68 dec 30"),
        // Division types the parameter from its own operand and then divides,
        // which is where the four places come from.
        ("SELECT ?/2", 1, "?/2 NewDecimal len 26 dec 4"),
        // Nothing on either side: both parameters are approximate.
        ("SELECT ?+?", 2, "?+? Double len 23 dec 31"),
        // A string beside it leaves it a string, and the arithmetic makes a
        // double of the pair anyway.
        ("SELECT ?+'x'", 1, "?+'x' Double len 23 dec 31"),
        // A string function keeps its argument's width, and two of them
        // together outgrow a VARCHAR and are described as a blob — which is
        // the same arithmetic the widths above are, not a second rule.
        (
            "SELECT CONCAT(?,?)",
            2,
            "CONCAT(?,?) MediumBlob len 524256 dec 31",
        ),
        (
            "SELECT CONCAT(?, 'x')",
            1,
            "CONCAT(?, 'x') MediumBlob len 262144 dec 31",
        ),
        ("SELECT UPPER(?)", 1, "UPPER(?) VarString len 65532 dec 31"),
        // A predicate over it is a predicate whatever it is over.
        ("SELECT ? IS NULL", 1, "? IS NULL LongLong len 1 dec 0"),
        // A conditional takes its type from the branch that *has* one, and
        // the parameter is then the widest of that kind — so an INT column
        // beside one comes out LONGLONG rather than LONG, because eight bytes
        // is what the parameter can carry. Typing the parameter a string here
        // instead, which is what it is on its own, described every one of
        // these VAR_STRING and told a binary client to decode an integer as
        // text.
        (
            "SELECT COALESCE(?, 1)",
            1,
            "COALESCE(?, 1) LongLong len 20 dec 0",
        ),
        (
            "SELECT COALESCE(?, 'x')",
            1,
            "COALESCE(?, 'x') VarString len 4096 dec 0",
        ),
        ("SELECT IF(1, ?, 2)", 1, "IF(1, ?, 2) LongLong len 20 dec 0"),
        (
            "SELECT CASE WHEN 1 THEN ? ELSE 1 END",
            1,
            "CASE WHEN 1 THEN ? ELSE 1 END LongLong len 20 dec 0",
        ),
        (
            "SELECT GREATEST(?, 1)",
            1,
            "GREATEST(?, 1) LongLong len 20 dec 0",
        ),
        // Nothing but parameters to take a type from, where a conditional
        // over nothing but written NULLs is `MYSQL_TYPE_NULL` — the two
        // spellings of "nothing decided" are not the same answer.
        (
            "SELECT GREATEST(?, ?)",
            2,
            "GREATEST(?, ?) VarString len 4096 dec 0",
        ),
        (
            "SELECT COALESCE(NULL, NULL)",
            0,
            "COALESCE(NULL, NULL) Null len 0 dec 0",
        ),
    ] {
        assert_eq!(
            described(sql, parameters),
            vec![expected.to_owned()],
            "{sql}"
        );
    }

    // Two parameters against a column, where the type comes from the column.
    assert_eq!(
        described("SELECT ?+i, ?+d, ?+f FROM P", 3),
        vec![
            "?+i LongLong len 22 dec 0".to_owned(),
            "?+d NewDecimal len 68 dec 30".to_owned(),
            "?+f Double len 23 dec 31".to_owned(),
        ]
    );

    // A parameter that is not projected changes nothing about the columns.
    assert_eq!(
        described("SELECT i FROM P WHERE i = ? AND s = ?", 2),
        vec!["i Long len 11 dec 0".to_owned()]
    );

    // The widest of the kind the column contributes, and not the column's own
    // code: an INT beside a parameter is a LONGLONG on 8.3.
    assert_eq!(
        described("SELECT COALESCE(?, i), IFNULL(?, i), LEAST(?, i) FROM P", 3),
        vec![
            "COALESCE(?, i) LongLong len 20 dec 0".to_owned(),
            "IFNULL(?, i) LongLong len 20 dec 0".to_owned(),
            "LEAST(?, i) LongLong len 20 dec 0".to_owned(),
        ]
    );

    // A temporal beside a parameter is read as the number arithmetic makes of
    // it, exactly as a literal 0 beside it already was: a `DATETIME` computes
    // exactly and a `DATE` computes whole. Reading it as the string a
    // parameter otherwise is made both of these a DOUBLE.
    assert_eq!(
        described("SELECT dt + ?, ? + dd FROM P", 2),
        vec![
            "dt + ? NewDecimal len 20 dec 0".to_owned(),
            "? + dd LongLong len 20 dec 0".to_owned(),
        ]
    );

    // 8.3 re-describes by the bound value's *type* at execute — measured
    // through `COM_STMT_EXECUTE`: `SELECT ?` bound to 7 comes back LONGLONG
    // and bound to 'abc' comes back VAR_STRING — and never renames. NULL is
    // the one binding that carries no type, and there 8.3 answers exactly what
    // it answered at prepare, which is what holds the two halves of this rule
    // together.
    let mut executed = |sql: &str, params: &[Value]| {
        let Ok(Outcome::Rows(rows)) = engine.execute(&mut s, sql, params) else {
            panic!("{sql} did not return rows");
        };
        rows.columns
            .iter()
            .map(|c| {
                let def = c.to_wire();
                format!("{} {:?} len {}", c.name, def.column_type, def.column_length)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        executed("SELECT ?", &[Value::Null]),
        vec!["? VarString len 65532".to_owned()]
    );
    assert_eq!(
        executed("SELECT ?", &[Value::Int(7)]),
        vec!["? LongLong len 2".to_owned()]
    );
}

/// A statement naming a table that is not there is refused at prepare.
///
/// Measured on 8.3 through `COM_STMT_PREPARE`: `SELECT * FROM NoSuch` is 1146
/// there, and this engine prepared it and described zero columns. A client
/// that prepares up front -- which Prisma does for every statement -- heard
/// nothing until it ran. #0244.
#[test]
fn a_missing_table_is_refused_at_prepare() {
    let (engine, session) = setup();
    for sql in [
        "SELECT * FROM NoSuch",
        "SELECT 1 FROM NoSuch",
        "SELECT (SELECT 1 FROM NoSuch) FROM Ty",
        "UPDATE NoSuch SET k = 1",
        "DELETE FROM NoSuch WHERE id = 1",
    ] {
        let refused = engine.describe(&session, sql, 0).unwrap_err();
        assert_eq!(refused.code, 1146, "{sql} -> {refused}");
    }
}

/// A write is resolved at prepare, even though it describes no columns.
///
/// 8.3 resolves the whole statement: `UPDATE Full SET k = 1 WHERE zz = 1` is
/// 1054 at prepare, not at execute. The engine described writes as `None`
/// without looking at them at all.
///
/// Resolved rather than probed, because the executor's own paths interleave
/// resolution with writing -- running one here would apply the statement.
#[test]
fn a_write_is_resolved_at_prepare_though_it_describes_nothing() {
    let (engine, mut session) = setup();
    engine
        .execute(&mut session, "CREATE TABLE Dm (k INT, id INT)", &[])
        .unwrap();

    for (sql, clause) in [
        ("UPDATE Dm SET k = 1 WHERE zz = 1", "where clause"),
        ("DELETE FROM Dm WHERE zz = 1", "where clause"),
        ("UPDATE Dm SET k = zz", "field list"),
    ] {
        let refused = engine.describe(&session, sql, 0).unwrap_err();
        assert_eq!(refused.code, 1054, "{sql} -> {refused}");
        assert!(
            refused.message.contains(clause),
            "{sql} named the wrong clause: {refused}"
        );
    }

    // And a write that resolves still describes no columns, which is what 8.3
    // answers for one: prepared, zero columns.
    for sql in [
        "UPDATE Dm SET k = 1 WHERE id = 1",
        "DELETE FROM Dm WHERE id = 1",
        "INSERT INTO Dm VALUES (1, 2)",
    ] {
        assert!(
            engine.describe(&session, sql, 0).unwrap().is_none(),
            "{sql} should prepare without describing a column"
        );
    }
}
