//! The SQL both servers are asked.
//!
//! Seeded from the places this engine has actually been wrong. Every entry
//! under "answers I got wrong" is a bug a reviewer caught by knowing MySQL;
//! having them here means the next one is caught by asking it.

pub struct Case {
    pub sql: &'static str,
}

/// Rows are compared by position, so a case without a total order is a case
/// that can differ because the two servers chose different plans. Every case
/// returning more than one row carries an `ORDER BY` that settles it.
pub struct Group {
    pub name: &'static str,
    pub setup: &'static [&'static str],
    pub cases: &'static [Case],
}

const fn same(sql: &'static str) -> Case {
    Case { sql }
}

pub const GROUPS: &[Group] = &[
    Group {
        name: "answers I got wrong",
        setup: &[
            "CREATE TABLE W (id INT NOT NULL, doc JSON NULL, name VARCHAR(40) NULL, \
             at DATETIME(3) NULL, approx DOUBLE NULL, PRIMARY KEY (id))",
            "INSERT INTO W VALUES (1, '{\"a\":[1,2],\"b\":\"s\"}', 'one', \
             '2026-05-02 14:07:09.100', 1.5)",
        ],
        cases: &[
            // Each of these was answered wrongly at some point, and the fix
            // was argued from memory rather than from the server.
            same("SELECT COUNT(*), SUM(1), MAX(2)"),
            same("SELECT JSON_OBJECT('q', JSON_QUOTE('hi'))"),
            same("SELECT DATE_ADD('2026-13-45', INTERVAL 1 DAY), YEAR('2026-13-45')"),
            same("SELECT DATE_ADD('2026-01-01', INTERVAL 1.5 DAY)"),
            same("SELECT DATE_ADD('2026-01-01', INTERVAL NULL DAY)"),
            same("SELECT HOUR('10:30:00'), MINUTE('10:30:00')"),
            same("SELECT DATE_ADD('2026-01-01 00:00:00.1', INTERVAL 1 SECOND)"),
            same("SELECT DATE_ADD('2026-01-31', INTERVAL 1 MONTH)"),
            same("SELECT DATE_ADD('2026-01-00', INTERVAL 1 DAY)"),
            same("SELECT DATE_ADD('2026-01-01', INTERVAL 999999999999999999 DAY)"),
            same("SELECT TIMESTAMPDIFF(DAY, '2026-01-01', '2026-03-05')"),
            same("SELECT approx + 0, approx * 2 FROM W"),
            same("SELECT doc + 0 FROM W WHERE id = 1"),
        ],
    },
    Group {
        name: "json",
        setup: &[
            "CREATE TABLE J (id INT NOT NULL, doc JSON NULL, txt VARCHAR(40) NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO J VALUES (1, '{\"a\":[1,2],\"b\":\"s\"}', '[9,9]')",
            "CREATE TABLE Norm (id INT, kept JSON)",
            "INSERT INTO Norm VALUES (1, '{\"b\":1,  \"a\":2}'), (2, '[ 1 , 2 ]')",
        ],
        cases: &[
            same("SELECT JSON_OBJECT('a', JSON_ARRAY(1, 2))"),
            same("SELECT JSON_ARRAY(1, 'x', NULL)"),
            same("SELECT JSON_OBJECT('d', doc), JSON_OBJECT('t', txt) FROM J"),
            same("SELECT JSON_EXTRACT(doc, '$.a'), doc->'$.a', doc->>'$.b' FROM J"),
            same("SELECT JSON_UNQUOTE(JSON_EXTRACT(doc, '$.b')) FROM J"),
            same("SELECT JSON_VALID(doc), JSON_TYPE(doc), JSON_LENGTH(doc) FROM J"),
            same("SELECT JSON_CONTAINS(doc, '1', '$.a') FROM J"),
            same("SELECT JSON_SET(doc, '$.c', 3) FROM J"),
            same("SELECT JSON_MERGE_PATCH(doc, JSON_OBJECT('a', 1)) FROM J"),
            same("SELECT CAST('[1,2]' AS JSON)"),
            same("SELECT JSON_KEYS(doc) FROM J"),
            same("SELECT JSON_ARRAYAGG(id) FROM J"),
            // MySQL orders an object's keys by byte length first and then
            // bytewise, where serde_json's map is lexicographic. So this is
            // `{"a": 2, "z": 1, "yy": 3}` and not `{"a":2,"yy":3,"z":1}` — a
            // different order, not only a different spacing.
            same("SELECT JSON_OBJECT('z', 1, 'a', 2, 'yy', 3, 'A', 4)"),
            same("SELECT CAST('{\"b\":1,\"a\":2,\"aa\":3,\"A\":4}' AS JSON)"),
            same("SELECT CAST('{}' AS JSON), CAST('[]' AS JSON), CAST('[[],{}]' AS JSON)"),
            same("SELECT CAST('[1.5, 2.0, 1e10, -0.0]' AS JSON)"),
            // How a double is written, which is a rule rather than a range:
            // fixed notation up to an exponent of 14 and from -15, and beyond
            // that only while the digits reach past the point — which is why
            // `1e15` is exponential and `1000000000000000.5` is not.
            same("SELECT CAST('[1e-7, 1e15, 1e30]' AS JSON)"),
            same("SELECT CAST('[1e13, 1e14, 1e15, 1e16, 1e308]' AS JSON)"),
            same("SELECT CAST('[1e-1, 1e-15, 1e-16, 1e-308]' AS JSON)"),
            same("SELECT CAST('[1.5e14, 1.5e15, 1000000000000000.5]' AS JSON)"),
            // JSON_UNQUOTE of a document is still that document's text, so it
            // has to be written the same way the document is.
            same("SELECT JSON_UNQUOTE(CAST('{\"a\":1}' AS JSON)), doc->>'$.a' FROM J"),
            // A JSON value compared with a string is false in MySQL whatever
            // the spelling: the string is read as a JSON string rather than
            // parsed, and an object never equals one. Here it is a text
            // comparison, so the canonical spelling wrongly matches. #0045.
            same("SELECT kept = '{\"a\": 2, \"b\": 1}', kept = '{\"b\":1,\"a\":2}' FROM Norm WHERE id = 1"),
            same("SELECT JSON_ARRAY('a/b', 'x\"y', 'a\\\\b')"),
            // A JSON column holds the document, not the text it was written
            // as, so reading it back normalises the spacing and the key order.
            same("SELECT kept FROM Norm ORDER BY id"),
        ],
    },
    Group {
        name: "aggregates and grouping",
        setup: &[
            "CREATE TABLE G (id INT NOT NULL, k INT NULL, v DECIMAL(10,2) NULL, \
             s VARCHAR(20) NULL, PRIMARY KEY (id))",
            "INSERT INTO G VALUES (1,1,'1.50','a'),(2,1,'2.50','b'),(3,2,NULL,NULL)",
        ],
        cases: &[
            same("SELECT k, COUNT(*), SUM(v), MIN(s), MAX(v), AVG(v) FROM G GROUP BY k ORDER BY k"),
            same("SELECT COUNT(*), COUNT(v), COUNT(DISTINCT k) FROM G"),
            same("SELECT k, GROUP_CONCAT(s ORDER BY s) FROM G GROUP BY k ORDER BY k"),
            same("SELECT k, COUNT(*) c FROM G GROUP BY k HAVING COUNT(*) > 1"),
            same("SELECT SUM(v) FROM G WHERE 1 = 0"),
            same("SELECT id, ROW_NUMBER() OVER (ORDER BY id) FROM G ORDER BY id"),
            same("SELECT id, SUM(id) OVER (PARTITION BY k ORDER BY id) FROM G ORDER BY id"),
            same("SELECT id, LAG(id, 1, 0) OVER (ORDER BY id) FROM G ORDER BY id"),
        ],
    },
    Group {
        // Everything here is about what the *binary* protocol sends, which the
        // harness could not see until it started asking both ways. Each case
        // is one where a declared type code narrower than the value it carries
        // would put the wrong bytes on the wire.
        name: "a type code has to hold its values",
        setup: &[
            "CREATE TABLE Q (t TINYINT, s SMALLINT, i INT, iu INT UNSIGNED, \
             bu BIGINT UNSIGNED, y YEAR, f FLOAT, d DOUBLE)",
            "INSERT INTO Q VALUES (100, 30000, 2000000000, 4000000000, 9000000000, 2026, \
             1.234567890123, 1.234567890123)",
            "INSERT INTO Q VALUES (100, 30000, 2000000000, 4000000000, 9000000000, 1901, \
             1e20, 1e20)",
            "INSERT INTO Q VALUES (100, 30000, 2000000000, 4000000000, 9000000000, 2155, \
             16777217, 16777217)",
        ],
        cases: &[
            // A total outgrows what it totals: three TINYINTs of 100 make 300,
            // and announcing the column as one byte sends 44.
            same("SELECT SUM(t), SUM(s), SUM(i) FROM Q"),
            same("SELECT AVG(t), MIN(t), MAX(t), COUNT(t) FROM Q"),
            // An unsigned column read back signed comes out negative.
            same("SELECT iu, bu FROM Q LIMIT 1"),
            // A FLOAT is four bytes and shows six significant digits; a DOUBLE
            // shows as many as it takes to read the value back. Neither is
            // what Rust prints, which never reaches for an exponent.
            same("SELECT f, d FROM Q ORDER BY f"),
            same("SELECT f + 0, d + 0 FROM Q ORDER BY f"),
            // Whether a FLOAT survives an expression is decided per function,
            // and the type and the value are decided in different code: MySQL
            // widens `-f` and `ABS(f)` to a double and keeps `COALESCE(f, 0)`
            // and `LEAST(f, 9)` a float. Every one of these is read twice
            // here, once per protocol, which is what catches the two
            // disagreeing.
            same("SELECT -f, ABS(f), ROUND(f, 2) FROM Q ORDER BY f"),
            same("SELECT COALESCE(f, 0), LEAST(f, 9), GREATEST(f, 0) FROM Q ORDER BY f"),
            same("SELECT CAST(f AS FLOAT), CAST(d AS FLOAT) FROM Q ORDER BY f"),
            // A float embeds in a document as the double it widens to, not as
            // the six digits the column displays: MySQL's JSON number is a
            // double.
            same("SELECT JSON_OBJECT('f', f), JSON_OBJECT('d', d) FROM Q ORDER BY f"),
            same("SELECT y FROM Q ORDER BY y"),
            // A union of the same width and different signs holds neither
            // branch's range.
            same("SELECT i FROM Q UNION SELECT iu FROM Q ORDER BY 1"),
            // And at *different* widths, which is the half the equal-width
            // pair above cannot see: whichever side is unsigned needs one size
            // more than itself to be held signed, and past BIGINT there is no
            // integer left at all.
            same("SELECT iu FROM Q UNION SELECT s FROM Q ORDER BY 1"),
            same("SELECT s FROM Q UNION SELECT iu FROM Q ORDER BY 1"),
            same("SELECT bu FROM Q UNION SELECT i FROM Q ORDER BY 1"),
            // The other order. Merging the two branches' signedness before
            // comparing it made an unsigned-first union match itself and keep
            // the narrower code, where this order never did — so the corpus
            // was covering only the half that worked.
            same("SELECT iu FROM Q UNION SELECT i FROM Q ORDER BY 1"),
        ],
    },
    Group {
        name: "types and description",
        setup: &[
            "CREATE TABLE T (
                 id INT NOT NULL AUTO_INCREMENT,
                 small SMALLINT NULL,
                 big BIGINT UNSIGNED NULL,
                 name VARCHAR(40) NOT NULL,
                 note TEXT NULL,
                 amt DECIMAL(10,2) NULL,
                 approx DOUBLE NULL,
                 r REAL NULL,
                 at DATETIME(3) NULL,
                 d DATE NULL,
                 kind ENUM('x','y') NULL,
                 flag BOOLEAN NULL,
                 PRIMARY KEY (id))",
            "INSERT INTO T (small, big, name, note, amt, approx, r, at, d, kind, flag) \
             VALUES (5, 9000000000, 'n', 'note', '1.25', 2.5, 1.234567890123, \
             '2026-01-02 03:04:05.678', '2026-01-02', 'x', 1)",
            "CREATE VIEW V AS SELECT id, small, name, amt, at, kind, flag FROM T",
        ],
        cases: &[
            same("SELECT * FROM T"),
            same("SELECT * FROM V"),
            same(
                "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, IS_NULLABLE, \
                 CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION, NUMERIC_SCALE, DATETIME_PRECISION \
                 FROM information_schema.columns \
                 WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'T' ORDER BY ORDINAL_POSITION",
            ),
            same(
                "SELECT COLUMN_NAME, COLUMN_TYPE, NUMERIC_PRECISION, CHARACTER_MAXIMUM_LENGTH \
                 FROM information_schema.columns \
                 WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'V' ORDER BY ORDINAL_POSITION",
            ),
        ],
    },
    Group {
        // What a client is told about NULL. A typed client models a column it
        // is told is nullable as optional, so a `NOT NULL` reported as
        // nullable is the wrong shape in generated code.
        name: "nullability",
        setup: &[
            "CREATE TABLE N (id INT NOT NULL, nn VARCHAR(9) NOT NULL, ny VARCHAR(9) NULL, \
             inn INT NOT NULL, tn TEXT NOT NULL, dn DATETIME NOT NULL, \
             en ENUM('a') NOT NULL, jn JSON NOT NULL, bn BLOB NOT NULL, PRIMARY KEY (id))",
            "CREATE TABLE M (aid INT NULL)",
            "INSERT INTO N VALUES (1,'a','b',2,'t','2026-01-01 00:00:00','a','{}','x')",
            // An unmatched row, so the outer joins actually pad something —
            // without it the NULL half of those cases never happens.
            "INSERT INTO M VALUES (1), (99)",
        ],
        cases: &[
            // Every type, because the flag used to be set only for the
            // numeric codes and so only the numeric columns were right.
            same("SELECT * FROM N"),
            // The preserved side of an outer join keeps NOT NULL; the
            // null-supplying side loses it, because the join can invent a row
            // of NULLs for it.
            same("SELECT nn, inn FROM N LEFT JOIN M ON M.aid = N.id"),
            same("SELECT M.aid, N.nn FROM M LEFT JOIN N ON N.id = M.aid"),
            // A union is nullable if either branch is.
            same("SELECT nn FROM N UNION SELECT ny FROM N"),
            same("SELECT nn FROM N UNION SELECT nn FROM N"),
            // A RIGHT JOIN pads the *left* side, which is the opposite of what
            // the code relaxed — so the engine flagged a column NOT NULL and
            // then sent NULL in it.
            same("SELECT id, nn FROM N RIGHT JOIN M ON M.aid = N.id ORDER BY id"),
            same("SELECT * FROM N RIGHT JOIN M ON M.aid = N.id ORDER BY id"),
            same("SELECT * FROM N LEFT JOIN M ON M.aid = N.id ORDER BY id"),
            // A literal branch is not nullable, and treating every computed
            // column as if it were imported a nullability that does not exist.
            same("SELECT id FROM N UNION SELECT 1 ORDER BY 1"),
            same("SELECT id FROM N UNION SELECT 1 + 1 ORDER BY 1"),
            same("SELECT id FROM N UNION ALL SELECT 2 ORDER BY 1"),
            same("SELECT id FROM N UNION SELECT NULL ORDER BY 1"),
            // Every kind, not just the integer: a NULL branch contributes no
            // type at all, so whatever the other branch is has to survive it.
            same("SELECT nn FROM N UNION SELECT NULL ORDER BY 1"),
            same("SELECT dn FROM N UNION SELECT NULL ORDER BY 1"),
            // `SELECT NULL UNION SELECT NULL` is deliberately absent. MySQL
            // answers it two ways — `NULL` when the statement is prepared and
            // `STRING` when it is run, because a materialised column cannot be
            // the null type — so it cannot tell an engine bug from a MySQL
            // inconsistency. The engine's answer is pinned by a unit test
            // instead. #0073.
            // NULL on the left, which is the branch that has to *adopt* the
            // other side's type rather than keep its own.
            same("SELECT NULL UNION SELECT id FROM N ORDER BY 1"),
        ],
    },
    Group {
        name: "joins and set operators",
        setup: &[
            "CREATE TABLE A (id INT NOT NULL, k INT NULL, PRIMARY KEY (id))",
            "CREATE TABLE B (id INT NOT NULL, aid INT NULL, label VARCHAR(12) NULL, PRIMARY KEY (id))",
            "INSERT INTO A VALUES (1,1),(2,2),(3,NULL)",
            "INSERT INTO B VALUES (1,1,'p'),(2,2,'q'),(3,9,'r')",
        ],
        cases: &[
            same("SELECT A.id, B.label FROM A JOIN B ON B.aid = A.id ORDER BY A.id"),
            same("SELECT A.id, B.label FROM A LEFT JOIN B ON B.aid = A.id ORDER BY A.id"),
            same("SELECT A.id FROM A WHERE A.id IN (SELECT aid FROM B) ORDER BY A.id"),
            same("SELECT id FROM A UNION SELECT id FROM B ORDER BY id"),
            same("SELECT id FROM A UNION ALL SELECT id FROM B ORDER BY id"),
            same("SELECT id FROM A INTERSECT SELECT id FROM B ORDER BY id"),
            same("SELECT id FROM A EXCEPT SELECT id FROM B ORDER BY id"),
            same("SELECT d.id FROM (SELECT id FROM A) d ORDER BY d.id"),
            same("WITH c AS (SELECT id FROM A) SELECT id FROM c ORDER BY id"),
        ],
    },
    Group {
        name: "writes and constraints",
        setup: &[
            "CREATE TABLE P (id INT NOT NULL, name VARCHAR(20) NOT NULL, PRIMARY KEY (id), \
             UNIQUE KEY uq (name))",
            "CREATE TABLE C (id INT NOT NULL, pid INT NOT NULL, PRIMARY KEY (id), \
             FOREIGN KEY (pid) REFERENCES P(id) ON DELETE CASCADE)",
            "INSERT INTO P VALUES (1,'a'),(2,'b'),(3,'c')",
            "INSERT INTO C VALUES (10,1),(20,2)",
        ],
        cases: &[
            same("INSERT INTO P VALUES (1, 'dup')"),
            same("INSERT INTO P VALUES (9, 'a')"),
            same("INSERT INTO C VALUES (99, 404)"),
            same("UPDATE P SET name = 'b' WHERE id = 1"),
            same("DELETE FROM P WHERE id = 1"),
            same("SELECT id FROM C ORDER BY id"),
            same("UPDATE P SET name = CONCAT(name, '!') ORDER BY id DESC LIMIT 1"),
            same("DELETE FROM P ORDER BY id LIMIT 1"),
            same("SELECT id, name AS after_dml FROM P ORDER BY id"),
            same("INSERT INTO P VALUES (4,'d') ON DUPLICATE KEY UPDATE name = 'e'"),
            same("SELECT id, name FROM P ORDER BY id"),
        ],
    },
    Group {
        // MySQL returns NULL from a scalar function if any argument is NULL,
        // and the exceptions are few enough to name. This engine used to read
        // a NULL as an empty string in most of them, so `UPPER(NULL)` was `''`
        // and `WHERE UPPER(t) = ''` matched rows MySQL does not return.
        name: "null propagates through functions",
        setup: &[],
        cases: &[
            same("SELECT UPPER(NULL), LOWER(NULL), UCASE(NULL), LCASE(NULL)"),
            same("SELECT TRIM(NULL), LTRIM(NULL), RTRIM(NULL), REVERSE(NULL), SPACE(NULL)"),
            same("SELECT LENGTH(NULL), CHAR_LENGTH(NULL), OCTET_LENGTH(NULL)"),
            same("SELECT LEFT(NULL, 2), RIGHT(NULL, 2), SUBSTRING(NULL, 1, 2), MID(NULL, 1, 2)"),
            same("SELECT LPAD(NULL, 3, 'x'), RPAD(NULL, 3, 'x'), REPLACE(NULL, 'a', 'b')"),
            same("SELECT INSTR(NULL, 'a'), LOCATE('a', NULL), CONCAT(NULL, 'a')"),
            same("SELECT MD5(NULL), DATE(NULL), DATE_FORMAT(NULL, '%Y'), DATEDIFF(NULL, '2026-01-01')"),
            same("SELECT YEAR(NULL), MONTH(NULL), DAY(NULL), HOUR(NULL), QUARTER(NULL)"),
            same("SELECT ABS(NULL), ROUND(NULL, 1), MOD(NULL, 2), GREATEST(NULL, 1), LEAST(NULL, 1)"),
            same("SELECT JSON_VALID(NULL), JSON_TYPE(NULL), JSON_QUOTE(NULL), JSON_LENGTH(NULL)"),
            // `UNIX_TIMESTAMP()` means now and `UNIX_TIMESTAMP(NULL)` means
            // NULL: the rule keys on having a NULL argument, not on the name,
            // so the no-argument form is untouched. This one used to answer
            // with the current time.
            same("SELECT UNIX_TIMESTAMP(NULL), UNIX_TIMESTAMP() > 0"),
            // The exceptions. Each of these means something with a NULL, and
            // propagating would break it.
            same("SELECT COALESCE(NULL, 1), IFNULL(NULL, 1), IF(NULL, 1, 2), ISNULL(NULL)"),
            same("SELECT NULLIF(NULL, 1), NULLIF(1, NULL), CONCAT_WS('-', NULL, 'a')"),
            // CONCAT_WS skips NULL *values*, but a NULL separator makes the
            // whole call NULL — exempting the function wholesale got this
            // wrong and answered 'ab'.
            same("SELECT CONCAT_WS(NULL, 'a', 'b'), IF(1, NULL, 2), COALESCE(NULL, NULL)"),
            same("SELECT JSON_ARRAY(NULL), JSON_OBJECT('k', NULL)"),
            same("SELECT JSON_SET('{}', '$.k', NULL), JSON_ARRAY_APPEND('[1]', '$', NULL)"),
            // A NULL *path* is not a NULL value: these are NULL, where the two
            // above store a JSON null. Exempting the function wholesale sent
            // the NULL to the path parser and failed with 3143.
            same("SELECT JSON_SET('{\"a\":1}', NULL, 2), JSON_ARRAY_APPEND('[1]', NULL, 2)"),
            same("SELECT SOUNDEX(NULL)"),
            // That a function this engine does not implement *says so* rather
            // than answering NULL is checked in `crates/engine/tests/sql.rs`
            // and not here: MySQL implements it, so a case for one could only
            // ever be a divergence. This line read `SOUNDEX(NULL)` for that
            // purpose until soundex was implemented, at which point it stopped
            // asking anything — it is kept for the NULL rule itself.
        ],
    },
    Group {
        name: "strings, numbers and null handling",
        setup: &[
            "CREATE TABLE S (id INT NOT NULL, t VARCHAR(30) NULL, n INT NULL, PRIMARY KEY (id))",
            "INSERT INTO S VALUES (1,'Hello',5),(2,NULL,NULL),(3,'  pad  ',-2)",
        ],
        cases: &[
            same("SELECT CONCAT(t, '!'), UPPER(t), LOWER(t), LENGTH(t) FROM S ORDER BY id"),
            same("SELECT TRIM(t), SUBSTRING(t, 2, 3), REPLACE(t, 'l', 'L') FROM S ORDER BY id"),
            // Where MySQL's Soundex parts company with the description of it:
            // a vowel does not break a run, the first letter seeds that run,
            // and the answer is not truncated to four.
            same("SELECT SOUNDEX('Tymczak'), SOUNDEX('Pfister'), SOUNDEX('Ashcraft')"),
            same("SELECT SOUNDEX('Robert'), SOUNDEX('Bhbb'), SOUNDEX(''), SOUNDEX(123)"),
            same("SELECT COALESCE(t, 'none'), IFNULL(n, 0), NULLIF(n, 5) FROM S ORDER BY id"),
            same("SELECT ROUND(n / 3, 2), ABS(n), n DIV 2, n % 3 FROM S ORDER BY id"),
            // `MOD` is `%` spelled as a keyword, and the parser is given the
            // symbol instead. That is only sound if the two are one operator
            // at one precedence, so the grouping is asked about here — and
            // the lowercase spelling, because an unaliased column is named
            // after the text the client wrote and not the text that parsed.
            same("SELECT n MOD 3, MOD(n, 3), n mod 2 + 1 FROM S ORDER BY id"),
            same("SELECT 1 + 8 MOD 3, 8 MOD 5 * 2, 17 MOD 7 MOD 2, 7.5 MOD 2, 7 MOD 0"),
            same("SELECT t LIKE 'H%', t REGEXP '^H', t IS NULL FROM S ORDER BY id"),
            same("SELECT CAST(n AS CHAR), CAST(t AS UNSIGNED) FROM S ORDER BY id"),
            // A cast to DECIMAL carries the scale it declares, filling in
            // (10,0) where it writes none, and reads a number out of text that
            // is not a decimal at all. The engine handed the text straight
            // back: `CAST('-2.5' AS DECIMAL)` was -2.5 where MySQL says -3.
            same("SELECT CAST('3.75' AS DECIMAL(10,1)), CAST('-2.5' AS DECIMAL), \
                  CAST('abc' AS DECIMAL(10,2)), CAST('1e3' AS DECIMAL(6,2))"),
            // MySQL's DECIMAL holds 65 digits and this engine counts 38, so
            // past that the digits are kept rather than folded through an
            // f64 — which answered `i128::MAX` for a number nothing asked for.
            same("SELECT CAST('123456789012345678901234567890123456789012' AS DECIMAL(65,0)), \
                  CAST('12345678901234567890123456789012345678.12' AS DECIMAL(65,2))"),
            same("SELECT CAST(n AS DECIMAL(8,2)), CAST(t AS DECIMAL(6,1)) FROM S ORDER BY id"),
            same("SELECT id FROM S WHERE n > 0 ORDER BY id"),
            same("SELECT id FROM S WHERE t = 'hello' ORDER BY id"),
            same("SELECT MD5('x'), LEFT(t, 2), RIGHT(t, 2) FROM S ORDER BY id"),
        ],
    },
    Group {
        // MySQL names an unaliased column after its source text everywhere
        // except here. A literal is named as it is read, so a string comes
        // back named by its *contents* and the quotes are gone -- and unary
        // plus and parentheses hand the literal straight back, so they are
        // gone too. #0059. The harness compares column names, which is the
        // only thing most of these are asking about.
        name: "how a literal names its column",
        setup: &[
            "CREATE TABLE L (id INT NOT NULL, t VARCHAR(30) NULL, PRIMARY KEY (id))",
            "INSERT INTO L VALUES (1, 'x')",
            // A view is stored as text and re-run on read, so it is the one
            // path where a name has to survive being written back out.
            "CREATE VIEW LV AS SELECT 'a' 'b'",
            "CREATE VIEW LP AS SELECT +'abc'",
            "CREATE VIEW LE AS SELECT 'a''b'",
            "CREATE VIEW LU AS SELECT 'a' 'b' UNION SELECT 'c'",
            // Names an identifier cannot hold without escaping. The first
            // doubles cleanly and is pinned into the definition; the second
            // does not survive being written back and falls back to the
            // unpinned text. Both have to come out named the way MySQL names
            // them, which is what makes the guard worth having.
            // A wildcard over something further down: the view's own
            // projection has nothing to pin, so the name has to survive
            // inside the derived table and the CTE instead.
            "CREATE VIEW LD AS SELECT * FROM (SELECT 'a' 'b') d",
            "CREATE VIEW LC AS WITH c AS (SELECT 'a' 'b') SELECT * FROM c",
            "CREATE VIEW LB AS SELECT 'a`b'",
            "CREATE VIEW LBB AS SELECT 'a``b'",
        ],
        cases: &[
            same("SELECT 'abc', \"abc\", '', 'it''s'"),
            // The escape is decoded into the name: the second column here is
            // named with a real newline in it.
            same("SELECT 'a\\'b', 'a\\nb', N'a'"),
            // An adjacent run is one string, named after the first of them.
            same("SELECT 'a' 'b' 'c', 'a'  'b', ('a' 'b'), 'a' 'b' = 'ab'"),
            // Leading characters that are not printable ASCII go, trailing
            // ones stay, and a name can end up empty.
            same("SELECT '  a', 'a  ', '   ', ' a '"),
            same("SELECT ('abc'), (('abc')), +'abc', + +'abc', +('abc')"),
            // A number is named by the token as written, and the plus in
            // front of it is not part of that token.
            same("SELECT +1, + 1, ++1, +1.5, +.5, +1e3, +1E3"),
            same("SELECT (1), ( +1 ), +( 1 ), (1+2), +(1+2)"),
            // The one literal MySQL renames rather than quoting back.
            same("SELECT NULL, null, NuLl, +NULL, (null)"),
            // The boundary. Unary minus is an operator MySQL records, so
            // these are the source text again, spacing included.
            same("SELECT -1, - 1, -(1), +-1, -+1"),
            // A keyword boolean carries no parse-time name at all, which is
            // why the plus stays on it where it comes off a number.
            //
            // `X'41'` and `0x41` belong beside these and are not here: MySQL
            // reports a hex literal's column UNSIGNED and this engine reports
            // it signed, and `-'abc'` is a DOUBLE `-0` there against a string
            // here. Both are real divergences and neither is about naming, so
            // they are covered in `crates/engine/tests/sql.rs` rather than
            // parked in a baseline this file says only ever shrinks.
            same("SELECT TRUE, true, +TRUE"),
            same("SELECT 'a' = 'a', 'a' IS NULL, CONCAT('a'), BINARY 'a'"),
            same("SELECT t, 'abc', +'abc' FROM L ORDER BY id"),
            // A run whose first literal is escaped: the doubled quote and the
            // appended literal cancel out in the lengths, so only the text
            // separates a run from a literal that merely looks like one.
            same("SELECT 'a''a' 'b', 'a\\'a' 'b', 'a' 'a''a'"),
            same("SELECT '' 'x', '' '', 'aa' ''"),
            same("SELECT * FROM (SELECT 'abc') d"),
            same("SELECT * FROM LV"),
            same("SELECT * FROM LP"),
            same("SELECT * FROM LE"),
            // A view over `1 MOD 3` belongs here too and is not here: MySQL
            // materialises that column as INT and this engine as BIGINT,
            // which is about a view's shape rather than about naming. It is
            // covered by name in `crates/engine/tests/sql.rs`.
            same("SELECT * FROM LU ORDER BY 1"),
            same("SELECT * FROM LD"),
            same("SELECT * FROM LC"),
            same("SELECT * FROM LB"),
            same("SELECT * FROM LBB"),
            same("WITH c AS (SELECT +'abc') SELECT * FROM c"),
            same("SELECT 'a' UNION SELECT 'b'"),
            // Past the 255 bytes MySQL will report a generated name in, both
            // for a literal's contents and for a call's source text.
            same("SELECT 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'"),
            same("SELECT CONCAT('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')"),
            // 256 bytes as 128 two-byte characters, so the cut lands inside
            // one: MySQL reports 127 of them rather than half of the 128th.
            same("SELECT 'éééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééé'"),
        ],
    },
    Group {
        name: "bitwise operators",
        setup: &[
            "CREATE TABLE B (id INT NOT NULL, n INT NULL, neg INT NULL, u BIGINT UNSIGNED NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO B VALUES (1, 5, -1, 12), (2, NULL, NULL, NULL), (3, 12, -6, 255)",
            // Written as strings, because a decimal literal reaches a column
            // through an f64 here and the digits are the point of the table.
            "CREATE TABLE Bd (id INT NOT NULL, wide DECIMAL(20,1), one DECIMAL(10,1), \
             two DECIMAL(10,2), huge DECIMAL(30,0), near VARCHAR(30), part VARCHAR(30), \
             packed VARCHAR(30), noisy VARCHAR(30), clockish VARCHAR(30), dt DATE, \
             wide30 DECIMAL(40,30), carry DECIMAL(40,30), PRIMARY KEY (id))",
            "INSERT INTO Bd VALUES (1, '4503599627370496.5', '3.7', '2.55', \
             '999999999999999999999999999999', '1-2', '2026-01', '20260102', \
             '2026-01-02 x', '12:34', '2026-01-02', '1234567890.5', \
             '9.9999999999999999999999999999995')",
        ],
        cases: &[
            same("SELECT 7 ^ 3, 7 & 3, 7 | 3, 1 << 3, 16 >> 2, ~-1"),
            // MySQL computes these over 64 *unsigned* bits, so a negative
            // operand is its two's complement and a shift does not sign-extend.
            same("SELECT -1 & 1, -1 >> 1, ~-6, ~-1 & 255, 1 + 3 & ~-6"),
            // And the half that used to be refused with 1235, because the
            // answer was above `i64::MAX` and had no `Value` to be. #0069's
            // unsigned tag is what closed #0077.
            same("SELECT ~0, 1 << 63, -5 & -3, ~1, 0 | 18446744073709551615"),
            // The mixed contexts, which are the reason a tag was needed rather
            // than a wider integer: the digits alone would have printed right
            // in the first column and been wrong in every one after it.
            same("SELECT ~0 > 5, ~0 = 18446744073709551615, ~0 MOD 10, ~0 DIV 2, ~0 - 1"),
            same("SELECT CONCAT(~0), JSON_ARRAY(~0), CAST(~0 AS SIGNED), CAST(~0 AS CHAR)"),
            // A bitwise answer is a BIGINT UNSIGNED, so arithmetic over one
            // leaves the range the same way a column's does.
            same("SELECT ~0 + 1"),
            same("SELECT ~u FROM B ORDER BY id"),
            same("SELECT ~n FROM B ORDER BY id"),
            same("SELECT ~0 UNION SELECT 1"),
            // A shift count is unsigned and is not reduced: 64 shifts
            // everything out, and -1 arrives as a count of 2^64-1.
            same("SELECT 1 << 64, 1 << 100, 1 << -1, (-1 >> 1) >> 62"),
            // A number is rounded to the nearest integer, away from zero at a
            // half — 3.7 becomes 4 and -2.5 becomes -3.
            same("SELECT 3.7 & 1, 3.4 & 1, 2.5 & 3, -2.5 & 3, ~-2.5, 1.5 << 1"),
            // A string is not: it converts by its leading *integer*, so it
            // stops at the point and at the exponent where a number does not.
            same("SELECT '3' & 1, 'x' & 1, '3.7' | 0, '1e2' | 0, '-3' & 255, ' 5 ' | 0"),
            same("SELECT 5 & NULL, NULL << 1, 1 << NULL, ~NULL, NULL ^ 1"),
            // sqlparser gives `^` Postgres's precedence, below `+` and `*`,
            // where MySQL binds it tighter than either. Each of these is a
            // different answer under the two readings.
            same("SELECT 2 ^ 3 + 1, 1 + 2 ^ 3, 2 ^ 3 * 2, 2 ^ 3 % 3, 2 ^ 3 DIV 4"),
            same("SELECT 1 & 2 ^ 3, 1 ^ 2 | 3, 1 | 2 ^ 3, 1 << 2 ^ 3, 1 ^ 2 << 3"),
            // And it ties `<<` with `^` and puts `&` above both, where MySQL
            // has `+ -`, then `<< >>`, then `&`, then `|`.
            same("SELECT 1 << 2 & 3, 1 & 2 << 3, 4 >> 1 & 2, 2 & 4 >> 1, 1 >> 2 | 3"),
            same("SELECT 1 + 2 << 3, 1 << 2 + 3, 1 * 2 << 3, 1 << 2 * 3, 1 & 2 | 3"),
            // Left-associative, which a rebuild has to be told: each of these
            // means something else read right to left.
            same("SELECT 2 >> 1 << 3, 8 >> 1 >> 1, 1 - 2 - 3, 7 ^ 2 ^ 1"),
            // A tighter operator on *both* sides of a looser one, which no
            // single rotation reaches: deciding needs the left operand's final
            // root, and it does not have one until it has been corrected too.
            same("SELECT 2 ^ 3 + 2 ^ 3, 7 ^ 1 + 6 ^ 7, 1 + 2 ^ 3 ^ 7"),
            same("SELECT 1 << 3 & 7 << 3, 3 ^ 3 & 4 ^ 2, 6 & 5 >> 1 << 3, 1 + 2 * 3 ^ 4"),
            // Two corrections meeting: `DIV` swallows the rest of the
            // expression and `^` has to be taken back out of what it swallowed.
            same("SELECT 7 DIV 2 ^ 3 + 1, 1 & 2 ^ 3 + 4"),
            // `~` is a prefix and binds tighter than any of them, where
            // sqlparser reads its operand at the precedence of `+`.
            same("SELECT ~-3 * 2, ~-9 DIV 2, ~-9 % 3, ~-3 * 2 + 1, 3 ^ ~(-6) * 2"),
            // Nested, which needs the outer `~` corrected before the inner
            // one; and a `~` the walk declines to move, whose correction still
            // has to reach the rebuild. Written `~ ~` because sqlparser reads
            // `~~` as one token and refuses it.
            same("SELECT ~ ~ 1 * 7, ~ ~ 3 * 5 % 9, 1 + ~(-6) * 2"),
            // A `~` on the *left* of a chain, which the walk never reads: only
            // the rebuild can see the `* 2` is inside it.
            same("SELECT ~(-6) * 2 ^ 3, ~(-9) DIV 2 ^ 3"),
            same("SELECT n & 1, n | 8, n ^ 3, n << 1, n >> 1 FROM B ORDER BY id"),
            same("SELECT ~neg, neg & 7, id & 1, id << 2, u & 15 FROM B ORDER BY id"),
            same("SELECT id FROM B WHERE id & 1 = 1 ORDER BY id"),
            same("SELECT (7 ^ 3) = 4, (7 & 3) > 1, 7 ^ 3 IS NULL, (NULL & 1) IS NULL"),
            // A decimal operand is rounded on its own digits rather than
            // through a double: 4503599627370496.5 is the first tie a double
            // cannot hold, and answering ...496 for MySQL's ...497 puts every
            // bit below it on the wrong side. Past 64 bits it saturates at the
            // *signed* end, which is not where a string saturates.
            same("SELECT wide | 0, wide & 1, one | 0, two | 0, huge | 0 FROM Bd ORDER BY id"),
            same("SELECT CAST('3.7' AS DECIMAL(10,1)) | 0, CAST('2.5' AS DECIMAL(10,1)) | 0, \
                  CAST('-0.4' AS DECIMAL(10,1)) | 0"),
            // A literal with a point is a DECIMAL to MySQL and an f64 here, so
            // it is read off the token for the same reason. An exponent is a
            // DOUBLE to MySQL too and keeps the reading it had.
            same("SELECT 4503599627370496.5 | 0, 4503599627370496.5 & 1, 1e2 | 0, 2.675 | 0"),
            // A sign is a unary operator to the parser and part of the number
            // to MySQL, so the same digits have to read the same way with one.
            same("SELECT -4503599627370496.5 & 1, -4503599627370496.5 & 3, -3.7 & 255, \
                  +4503599627370496.5 & 1, +2.5 & 3"),
            // Signs are counted, not concatenated, and parentheses group a
            // number without retyping it.
            same("SELECT - -2.5 & 3, - - -2.5 & 3, -(-2.5) & 3, \
                  -(4503599627370496.5) & 1"),
            // A quoted string is a string however much it looks like a date,
            // and reads by its leading integer where the *typed* temporal
            // beside it reads its packed digits — see the temporal group below.
            same("SELECT '2026-01-02' | 0, '2026-01-02 03:04:05' | 0, '06:07:08' | 0, \
                  '2026' | 0, '20260102' | 0"),
            // And the same asked of columns, where no literal can decide it:
            // every one of these is a VARCHAR, so every one reads as a string
            // however much its characters look like a date or a clock.
            same("SELECT near | 0, part | 0, packed | 0, noisy | 0, clockish | 0 \
                  FROM Bd ORDER BY id"),
            // An expression whose *result* type the statement declares is a
            // string, and MySQL reads it as one however its argument was
            // typed — 2026 where the bare DATE column beside it is 20260102.
            same("SELECT CAST(dt AS CHAR) | 0, CONCAT(dt, '') | 0, LEFT(dt, 10) | 0, \
                  SUBSTRING(dt, 1, 10) | 0 FROM Bd ORDER BY id"),
            same("SELECT TRIM(dt) | 0, UPPER(dt) | 0, COALESCE(dt, '0') | 0, \
                  IFNULL(dt, '0') | 0 FROM Bd ORDER BY id"),
            same("SELECT CAST('2026-01-02' AS CHAR) | 0, CONCAT('2026-', '01-02') | 0, \
                  LEFT('2026-01-02 03:04:05', 10) | 0"),
            // The scale is reached exactly however many digits stand in front
            // of it: `DECIMAL(40,30)` overflows the counted rescaler at ten,
            // and the carry can run off the front of the number.
            same("SELECT wide30, carry FROM Bd ORDER BY id"),
            same("SELECT CAST('1.5' AS DECIMAL(65,30))"),
        ],
    },
    Group {
        // MySQL reads a temporal in a numeric context as its packed
        // `YYYYMMDD[HHMMSS]` integer, and the answer turns on the operand's
        // *type* rather than its characters: the DATE column and the VARCHAR
        // beside it hold the same text and convert to 20240105 and 2024.
        // Nothing about the text can decide that, which is why the engine
        // tags the value.
        name: "a temporal in a numeric context",
        setup: &[
            "CREATE TABLE Tm (id INT NOT NULL, d DATE NULL, dt DATETIME NULL, \
             dt3 DATETIME(3) NULL, tm TIME NULL, tm3 TIME(3) NULL, ts TIMESTAMP NULL, \
             y YEAR NULL, s VARCHAR(30) NULL, short DATETIME NULL, PRIMARY KEY (id))",
            // The second row is all NULL, so every case below also says what
            // the conversion does with one.
            "INSERT INTO Tm VALUES (1, '2024-01-05', '2024-01-05 06:07:08', \
             '2024-01-05 06:07:08.500', '06:07:08', '06:07:08.500', \
             '2024-01-05 06:07:08', 2024, '2024-01-05', '2024-01-05 06:07')",
            "INSERT INTO Tm (id) VALUES (2)",
            // A temporal is stored by its *type*, not by how it was written,
            // and every column here is given something the wrong shape for it:
            // a bare date into a DATETIME, a datetime into a DATE, and a
            // fraction into columns declared without one. The last is the
            // pairing that splits the two protocols, because it packs to a
            // decimal under a column described LONGLONG.
            "CREATE TABLE Shape (id INT NOT NULL, dt DATETIME NULL, d DATE NULL, \
             tm TIME NULL, tm1 TIME(1) NULL, dt0 DATETIME NULL, dt6 DATETIME(6) NULL, \
             roll TIME NULL, PRIMARY KEY (id))",
            "INSERT INTO Shape VALUES (1, '2024-01-05', '2024-01-05 06:07:08', \
             '06:07:08.567', '06:07:08.567', '2024-01-05 06:07:08.567', \
             '2024-01-05 06:07:08.567', '23:59:59.567')",
            "INSERT INTO Shape (id) VALUES (2)",
            // Written as strings, because a literal with an exponent reaches a
            // column through an f64 and the digits are the point of the table.
            "CREATE TABLE Exp (id INT NOT NULL, wide DECIMAL(65,30) NULL, \
             two DECIMAL(20,2) NULL, PRIMARY KEY (id))",
            "INSERT INTO Exp VALUES (1, '1e3', '1e3'), (2, '1.5e2', '-2.5e-1')",
        ],
        cases: &[
            // The table itself: a DATE is eight digits, a DATETIME fourteen, a
            // TIME six with its leading zero gone, and a YEAR the number it
            // already was.
            same("SELECT d + 0, dt + 0, tm + 0, ts + 0, y + 0 FROM Tm ORDER BY id"),
            // Fractional seconds pack after the point, so these are decimals
            // and keep every digit the column was declared with.
            same("SELECT dt3 + 0, tm3 + 0 FROM Tm ORDER BY id"),
            // A YEAR is the exception: MySQL computes in it directly, because
            // it already is a number. It is UNSIGNED there, which the others
            // are not.
            // `y - 1` is left out: MySQL reports it UNSIGNED and this engine
            // keeps subtraction off the unsigned list on purpose, because it
            // answers -1 where MySQL raises 1690. That is #0069, not this.
            same("SELECT y, y + 0, y * 1, -y FROM Tm ORDER BY id"),
            // The same number under every arithmetic operator. Unary minus is
            // a DOUBLE in MySQL where `d + 0` is a LONGLONG, and unary plus
            // is the identity and stays a DATE.
            // `+d` is aliased because MySQL names that column `d` and this
            // engine names it `+d`, which is a naming gap and not this one.
            same("SELECT d - 0, d * 1, d MOD 7, d DIV 1, -d, +d AS plus_d FROM Tm ORDER BY id"),
            same("SELECT -dt, -tm, dt * 1, tm * 1, ts - 0 FROM Tm ORDER BY id"),
            // A comparison against a number is numeric — the packed value —
            // and against a string is not.
            same("SELECT d = 20240105, d = '2024-01-05', dt = 20240105060708, \
                  tm = 60708, d > 2024 FROM Tm ORDER BY id"),
            same("SELECT id FROM Tm WHERE d > 20240101 ORDER BY id"),
            same("SELECT d BETWEEN 20240100 AND 20240110, d IN (20240105), \
                  d IN ('2024-01-05') FROM Tm ORDER BY id"),
            // A cast reads the packed number too, and rounds it: a DATETIME(3)
            // at .500 is ...709 AS SIGNED.
            same("SELECT CAST(d AS SIGNED), CAST(dt AS SIGNED), CAST(dt3 AS SIGNED), \
                  CAST(tm3 AS SIGNED) FROM Tm ORDER BY id"),
            same("SELECT CAST(d AS DECIMAL(20,2)), CAST(dt3 AS DECIMAL(30,6)), \
                  CAST(tm AS DECIMAL(20,0)) FROM Tm ORDER BY id"),
            // A cast to CHAR is the text, not the number, which is the whole
            // distinction the tag draws.
            same("SELECT CAST(d AS CHAR), CAST(dt AS CHAR), CONCAT(d, ''), \
                  CONCAT(dt3, '') FROM Tm ORDER BY id"),
            // An aggregate folds the packed numbers and answers a decimal.
            same("SELECT SUM(d), AVG(d), MAX(d), MIN(tm) FROM Tm"),
            same("SELECT SUM(dt), SUM(tm), SUM(dt3), SUM(y), MAX(y) FROM Tm"),
            same("SELECT d, COUNT(*) FROM Tm GROUP BY d ORDER BY d"),
            // Beside a string the answer is a double, and the string still
            // reads by its leading number.
            same("SELECT d + '1', d + s, s + 0, d + 'x' FROM Tm ORDER BY id"),
            // `COALESCE` and `IFNULL` aggregate their arguments' types, so a
            // string anywhere in them makes the temporal a string — 2024.
            // `IF`, `GREATEST` and `CASE` were measured and do not.
            same("SELECT COALESCE(d, '0') + 0, IFNULL(d, '0') + 0, COALESCE(d, 0) + 0, \
                  COALESCE(d, d) + 0 FROM Tm ORDER BY id"),
            same("SELECT IF(1, d, '0') + 0, GREATEST(d, '0') + 0, \
                  (CASE WHEN 1 THEN d ELSE '0' END) + 0 FROM Tm ORDER BY id"),
            // A temporal *expression* converts like a temporal column, which
            // is measured and not obvious: MySQL types
            // `DATE_ADD('2024-01-05', INTERVAL 1 DAY)` a string and still
            // reads it 20240106, where the bare literal reads 2024.
            same("SELECT DATE_ADD('2024-01-05', INTERVAL 1 DAY) + 0, \
                  DATE('2024-01-05 06:07:08') + 0, LAST_DAY('2024-01-05') + 0, \
                  '2024-01-05' + 0"),
            same("SELECT DATE_ADD(d, INTERVAL 1 DAY) + 0, ADDDATE(d, 1) + 0, \
                  DATE(dt) + 0 FROM Tm ORDER BY id"),
            same("SELECT CAST('2024-01-05' AS DATE) + 0, \
                  CAST('2024-01-05 06:07:08' AS DATETIME) + 0, \
                  CAST('06:07:08' AS TIME) + 0"),
            // A TIME keeps its sign and runs past 99 hours, so the packing
            // widens groups without truncating them.
            same("SELECT CAST('-06:07:08' AS TIME) + 0, CAST('838:59:59' AS TIME) + 0, \
                  CAST('-06:07:08' AS TIME) < 0"),
            // A DATETIME written without its seconds. MySQL reads the column
            // as 20240105060700, so the missing group packs as zero rather
            // than shortening the number.
            same("SELECT short + 0 FROM Tm ORDER BY id"),
            // The bitwise operators read the same packed number, rounded the
            // way any other decimal operand is.
            same("SELECT d | 0, dt | 0, tm | 0, dt3 | 0, tm3 | 0 FROM Tm ORDER BY id"),
            // And the numeric functions, which widen to a double.
            same("SELECT ABS(d), FLOOR(d), CEILING(d), TRUNCATE(d, 0) FROM Tm ORDER BY id"),

            // -- stored by the type, not by how it was written --------------
            //
            // A DATETIME given a bare date holds midnight, a DATE given a
            // datetime keeps only the date, and a fraction is rounded to the
            // column's declared precision — so `06:07:08.567` in a TIME is
            // `06:07:09`. Keeping the characters made the packed number a
            // property of the statement, and `d = '2024-01-05'` false for a
            // DATE the statement had written with a clock on it.
            same("SELECT dt, d, tm, tm1, dt0, dt6 FROM Shape ORDER BY id"),
            same("SELECT dt + 0, d + 0, tm + 0, tm1 + 0, dt0 + 0, dt6 + 0 \
                  FROM Shape ORDER BY id"),
            same("SELECT d = '2024-01-05', dt = '2024-01-05 00:00:00', tm = '06:07:09' \
                  FROM Shape ORDER BY id"),
            // The rounding carries, and a TIME has no day to spill into.
            same("SELECT roll, roll + 0 FROM Shape ORDER BY id"),
            // The cast declares a shape and a precision exactly as a column
            // does, and both are applied.
            same("SELECT CAST('06:07:08.567' AS TIME), CAST('06:07:08.567' AS TIME) + 0, \
                  CAST('06:07:08.567' AS TIME(1)), CAST('06:07:08.567' AS TIME(1)) + 0"),
            same("SELECT CAST(dt6 AS DATETIME), CAST(dt6 AS DATETIME) + 0, \
                  CAST(dt6 AS DATETIME(1)) + 0, CAST(dt AS DATE), \
                  CAST('2024-01-05' AS DATETIME) FROM Shape ORDER BY id"),
            // A shift keeps the precision of the argument's declared type,
            // which a string does not have: a DATETIME(3) column keeps three
            // digits and a string literal with a fraction is padded to six.
            same("SELECT DATE_ADD(dt3, INTERVAL 1 DAY), DATE_ADD(dt3, INTERVAL 1 DAY) + 0, \
                  DATE_ADD(dt, INTERVAL 1 SECOND) + 0 FROM Tm ORDER BY id"),
            same("SELECT DATE_ADD('2026-01-01 00:00:00.1', INTERVAL 1 SECOND) , \
                  DATE_ADD('2026-01-01', INTERVAL 1 SECOND), \
                  TIMESTAMPADD(SECOND, 1, dt3) FROM Tm ORDER BY id"),
            // Where the precision is carried through an expression rather than
            // read off a column: MySQL keeps it and so must the metadata, or
            // the value is a decimal under a column described LONGLONG.
            same("SELECT COALESCE(dt3, dt3) + 0, IFNULL(dt3, dt3) + 0, \
                  GREATEST(dt3, dt3) + 0, IF(1, dt3, dt3) + 0, \
                  (CASE WHEN 1 THEN dt3 ELSE dt3 END) + 0 FROM Tm ORDER BY id"),
            same("SELECT MAX(dt3) + 0, MIN(tm3) + 0 FROM Tm"),
            // `CURTIME(n)` takes a precision like `NOW(n)` does, and ignoring
            // it described the column a decimal and answered an integer. The
            // value moves, so its width is what is compared: twelve characters
            // with three digits of fraction and eight without.
            same("SELECT LENGTH(CURTIME(3)), LENGTH(CURTIME()), LENGTH(NOW(3)), LENGTH(NOW())"),

            // -- an exponent is spelled out, not folded through a float ------
            //
            // `1e3` at thirty places is a thousand and thirty zeros. Through
            // an `f64` it was 1000.000000000000089690419062898688, because a
            // double holds about fifteen of those digits and the scale asks
            // for twice that.
            same("SELECT wide, two FROM Exp ORDER BY id"),
            same("SELECT CAST('1e3' AS DECIMAL(65,30)), CAST('1e3' AS DECIMAL(20,2)), \
                  CAST('1.5e2' AS DECIMAL(10,3)), CAST('-2.5e-1' AS DECIMAL(10,4))"),
            // And text that is not a number at all still reads as the number
            // MySQL reads out of it, which is the path this shares.
            same("SELECT CAST('abc' AS DECIMAL(20,2)), CAST('12abc' AS DECIMAL(20,2)), \
                  CAST('1e' AS DECIMAL(20,2))"),
        ],
    },
    Group {
        // `sql_mode` is a session setting, and the harness restores it before
        // every group, so this one can turn a flag on and leave it on for its
        // cases. Written as an addition to the mode already in force rather
        // than as a mode of its own: replacing it would quietly drop
        // STRICT_TRANS_TABLES for this group and make its inserts mean
        // something else.
        name: "sql_mode: REAL_AS_FLOAT",
        setup: &[
            "SET SESSION sql_mode = CONCAT(@@sql_mode, ',REAL_AS_FLOAT')",
            // The flag moves what `REAL` is a synonym for. `DOUBLE PRECISION`
            // is not that synonym and does not move, which is the half a table
            // of REAL columns alone could not tell apart from a mode that
            // narrowed every float.
            "CREATE TABLE R (id INT NOT NULL, r REAL NULL, ru REAL UNSIGNED NULL, \
             dp DOUBLE PRECISION NULL, f FLOAT NULL, d DOUBLE NULL, PRIMARY KEY (id))",
            "INSERT INTO R VALUES (1, 1.234567890123, 2.5, 1.234567890123, \
             1.234567890123, 1.234567890123)",
        ],
        cases: &[
            same("SELECT r, ru, dp, f, d FROM R"),
            same(
                "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, NUMERIC_PRECISION, NUMERIC_SCALE \
                 FROM information_schema.columns \
                 WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'R' ORDER BY ORDINAL_POSITION",
            ),
            // A cast reads the flag too, and it is the only place the narrowing
            // can be seen without a table to have declared it.
            same("SELECT CAST(1.234567890123 AS REAL), CAST(1.234567890123 AS DOUBLE)"),
            // Arithmetic widens a four-byte column back to a double, so the
            // digits the column dropped stay dropped and the ones it kept are
            // shown in full.
            same("SELECT r + 0, r * 2, dp + 0 FROM R"),
        ],
    },
    Group {
        // MySQL gives an ENUM a number — the 1-based position it was declared
        // at — and this engine read the number out of the member's own text,
        // so every member that is not itself a number answered 0.
        //
        // Three questions, and the answers do not agree with each other:
        // arithmetic reads the ordinal, comparison reads the member, and the
        // *sort* reads the ordinal again. `r` is declared out of alphabetical
        // order so that those three cannot pass for one another.
        name: "an enum in a numeric context",
        setup: &[
            "CREATE TABLE E (id INT NOT NULL, e ENUM('a','b','c') NULL, \
             n ENUM('7','8','9') NULL, r ENUM('b','a') NULL, r2 ENUM('b','a') NULL, \
             PRIMARY KEY (id))",
            // `n`'s members are digits, which is the pairing that proves the
            // ordinal is not the text: `n` holding '7' reads 1 and not 7.
            "INSERT INTO E VALUES (1, 'a', '7', 'b', 'a'), (2, 'c', '9', 'a', 'b')",
            // An all-NULL row, so every case below also says what each of
            // these does with one.
            "INSERT INTO E (id) VALUES (3)",
            // No key, because every case runs twice — once through each
            // protocol — and the second insert of the same row must not be
            // refused for a reason that has nothing to do with ENUM.
            "CREATE TABLE E2 (v ENUM('a','b','c') NULL)",
            // Members that collide with their own ordinals, which is the only
            // shape where "a member wins over an index" and "a number is
            // always an index" can be told apart: `'2'` here is the *first*
            // member and `2` is the second.
            "CREATE TABLE E3 (v ENUM('2','1') NULL)",
            // The same members in a different order, so copying a value in
            // has to resolve it again rather than carry the position over.
            "CREATE TABLE E4 (v ENUM('c','b','a') NULL)",
            "CREATE TABLE E5 (t TEXT NULL, b BLOB NULL)",
            "INSERT INTO E5 VALUES ('x', 'x'), ('y', 'y')",
            // A leading number and a trailing tail, which is where the total
            // and the row it came from can disagree.
            "CREATE TABLE E6 (t TEXT NULL, b BLOB NULL)",
            "INSERT INTO E6 VALUES ('12abc', '12abc'), ('3.5x', '3.5x')",
            // A temporal whose *type* is not one: MySQL types `DATE_ADD` over
            // a VARCHAR as a string and computes it as a double.
            "CREATE TABLE E7 (id INT NOT NULL, v VARCHAR(40) NULL, dt6 DATETIME(6) NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO E7 VALUES (1, '2026-01-02 06:07:08.500000', \
             '2026-01-02 06:07:08.500000')",
        ],
        cases: &[
            // The five readings the issue was written from.
            same("SELECT id, e, e + 0, -e, e = 1, CAST(e AS SIGNED), e = 'a' FROM E ORDER BY id"),
            // Digits for members: 1 and 3, not 7 and 9.
            same("SELECT id, n, n + 0, n = '7', n = 1, n = 7 FROM E ORDER BY id"),
            same("SELECT id, e * 2, e DIV 1, e | 0, e & 3, e + 0.0 FROM E ORDER BY id"),
            same(
                "SELECT id, CAST(e AS DECIMAL(10,2)), CAST(e AS CHAR), CONCAT(e, '') \
                 FROM E ORDER BY id",
            ),
            same("SELECT SUM(e), AVG(e), COUNT(e), SUM(n), AVG(n) FROM E"),
            // MIN and MAX go with the *comparison*, so over `r` they answer
            // 'a' and 'b' where the sort below puts 'b' first — and the winner
            // comes back without its ordinal.
            same("SELECT MIN(r), MAX(r), MIN(r) + 0, MIN(e), MAX(e) FROM E"),
            // The sort goes with the ordinal: 'b' is the first member of `r`.
            same("SELECT id, r FROM E WHERE r IS NOT NULL ORDER BY r, id"),
            same("SELECT id, r FROM E WHERE r IS NOT NULL ORDER BY r DESC, id"),
            same("SELECT r, COUNT(*) FROM E GROUP BY r ORDER BY r"),
            same("SELECT DISTINCT r FROM E ORDER BY r"),
            // HAVING and an ORDER BY expression both name the projected value.
            // Reaching it as the string it renders as, `HAVING r = 1` compared
            // a member against a number and returned no group, while the
            // SELECT beside it read the ordinal.
            same("SELECT r, COUNT(*) FROM E GROUP BY r HAVING r = 1"),
            same("SELECT r, COUNT(*) FROM E GROUP BY r HAVING r + 0 = 1"),
            same("SELECT r, COUNT(*) FROM E GROUP BY r ORDER BY r + 0 DESC"),
            same("SELECT e, COUNT(*) FROM E GROUP BY e HAVING e > 1 ORDER BY e"),
            // Two ENUM columns of the same declaration compare by their
            // members and not by their ordinals: 'b' beside 'a' is greater
            // here, where the ordinals 1 and 2 would make it smaller.
            same("SELECT id, r, r2, r = r2, r < r2, r > r2 FROM E ORDER BY id"),
            // A member string is compared as a string and a number as the
            // ordinal, in the same row.
            same(
                "SELECT id, r < 'a', r > 'a', r = 'zzz', r IN ('a','b'), r IN (1,2), \
                 r BETWEEN 1 AND 2 FROM E ORDER BY id",
            ),
            // Truthiness follows the ordinal, so every real member is true.
            same("SELECT id FROM E WHERE e ORDER BY id"),
            // Which functions carry the ordinal through and which do not.
            // `COALESCE` drops it with one argument and nothing to aggregate
            // against, where `IF` beside it keeps it.
            same(
                "SELECT COALESCE(e) + 0, IFNULL(e, 'z') + 0, IF(1, e, 'z') + 0, \
                 NULLIF(e, 'z') + 0, GREATEST(e, e) + 0, LEAST(e, e) + 0 \
                 FROM E WHERE id = 1",
            ),
            same(
                "SELECT (CASE WHEN 1 THEN e END) + 0, (SELECT e FROM E WHERE id = 1) + 0 \
                 FROM E WHERE id = 1",
            ),
            same("SELECT x + 0 FROM (SELECT e AS x FROM E WHERE id = 1) d"),
            // A document embeds the member, not the ordinal.
            same("SELECT JSON_OBJECT('a', e), JSON_ARRAY(e) FROM E WHERE id = 1"),
            // Assignment reads the same way round. A member wins over an
            // index, a number is an index, and a string is an index only when
            // it is a whole number.
            same("INSERT INTO E2 (v) VALUES (2)"),
            same("INSERT INTO E2 (v) VALUES ('2')"),
            same("INSERT INTO E2 (v) VALUES ('A')"),
            same("INSERT INTO E2 (v) VALUES (1.6)"),
            same("INSERT INTO E2 (v) VALUES (NULL)"),
            // Index 0 is the error member, the empty string — and only a
            // *string* reaches it. The number 0 is refused.
            same("INSERT INTO E2 (v) VALUES ('0')"),
            same("INSERT INTO E2 (v) VALUES (0)"),
            same("INSERT INTO E2 (v) VALUES (9)"),
            same("INSERT INTO E2 (v) VALUES ('zzz')"),
            same("INSERT INTO E2 (v) VALUES ('1.0')"),
            // Ordered by the ordinal, and every row sharing one is the same
            // row, so the order is total.
            same("SELECT v, v + 0, LENGTH(v) FROM E2 ORDER BY v"),
            same("INSERT INTO E3 (v) VALUES ('2')"),
            same("INSERT INTO E3 (v) VALUES (2)"),
            same("SELECT v, v + 0 FROM E3 ORDER BY v"),
            // The error member has to survive being read back out and
            // written again, into a declaration with the same members in a
            // different order. The `UPDATE t SET v = v` half of this is in
            // the engine's own tests instead: MySQL reports the rows it
            // *changed* and this engine reports the rows it matched, which is
            // a divergence of its own and not one about ENUM.
            same("INSERT INTO E4 SELECT v FROM E2"),
            same("SELECT v, v + 0, LENGTH(v) FROM E4 ORDER BY v"),
            // A total over a column where nothing reads as a number is 0,
            // not -0 — and a binary string is read the same way text is.
            same("SELECT SUM(t), AVG(t), SUM(t) = 0, SUM(b), AVG(b) FROM E5"),
            // A total reads each value by its leading number, the same rule
            // the row beside it reads by: 12 and 3.5, so 15.5 and not 0.
            same("SELECT SUM(t), AVG(t), SUM(b), AVG(b) FROM E6"),
            same("SELECT t + 0, b + 0 FROM E6 ORDER BY t"),
            // The value has to follow the code it is announced under: MySQL
            // types this DOUBLE for the VARCHAR subject and NEWDECIMAL for
            // the DATETIME(6) one, and the engine packs both exactly — so the
            // first sent every declared digit through the text protocol and a
            // rounded double through the binary one.
            same(
                "SELECT SUM(DATE_ADD(v, INTERVAL 1 DAY)), \
                 SUM(DATE_ADD(dt6, INTERVAL 1 DAY)) FROM E7",
            ),
            // The VARCHAR subject only. The DATETIME(6) one beside it is a
            // NEWDECIMAL in both, and diverges on the *scale* a decimal
            // multiplication keeps — 30390154591062.7500000 against
            // 30390154591062.75 — which is #0070 and not this.
            same("SELECT DATE_ADD(v, INTERVAL 1 DAY) * 1.5 FROM E7"),
            // A set operation materialises its result, and an ENUM does not
            // survive one: the column is VAR_STRING, `x + 0` is 0 and the
            // order is by the member — with the same declaration on both
            // sides as well as with two different ones, whose ordinals would
            // otherwise have met in one sort key meaning different members.
            same("SELECT x FROM (SELECT r AS x FROM E UNION ALL SELECT e AS x FROM E) d ORDER BY x"),
            same(
                "SELECT x, x + 0 FROM (SELECT r AS x FROM E UNION ALL SELECT r AS x FROM E) d \
                 ORDER BY x",
            ),
            // The bare set operation as well as the derived table, because
            // only the bare one reports what the operation itself decided: a
            // derived table's outer projection re-derives the column type.
            same("SELECT r FROM E UNION ALL SELECT e FROM E ORDER BY r"),
            same("SELECT r AS m FROM E UNION SELECT e AS m FROM E ORDER BY m"),
        ],
    },
    Group {
        // MySQL reads the keyword TRUE as the JSON value `true` and the
        // literal 1 as the JSON number 1, so the two compare against different
        // documents and only one of the four pairings below differs. This
        // engine had no boolean value, so both sides evaluated to 1 and it
        // answered the numeric row twice.
        //
        // The keyword is the smaller half. Every comparison, logical operator,
        // postfix test and predicate function is typed boolean there too, and
        // the tag survives only inside the expression that asked the question
        // — a column, a derived table, a CTE, a set operation or a user
        // variable all take it off, which is what keeps a stored 1 from
        // suddenly matching a document of `true`.
        name: "a boolean beside a document",
        setup: &[
            // `f` is spelled BOOLEAN and `t` TINYINT(1) because MySQL makes
            // them the same column; both are here so the corpus says so.
            "CREATE TABLE B (id INT NOT NULL, f BOOLEAN NULL, t TINYINT(1) NULL, \
             n INT NULL, j JSON NULL, PRIMARY KEY (id))",
            "INSERT INTO B VALUES (1, TRUE, 1, 1, CAST('true' AS JSON)), \
             (2, FALSE, 0, 0, CAST('false' AS JSON))",
            // No key, so the second run of the same case through the other
            // protocol is not refused for a reason unrelated to booleans.
            "CREATE TABLE BE (v ENUM('2','1') NULL)",
            // A variable is somewhere to put a value, and the tag does not
            // survive being put anywhere. Set here rather than in a case so
            // both protocols read the same one.
            "SET @b = TRUE",
        ],
        cases: &[
            // The four rows of #0061, and the same four for FALSE.
            same("SELECT CAST('true' AS JSON) = TRUE, CAST('true' AS JSON) = 1"),
            same("SELECT CAST('1' AS JSON) = TRUE, CAST('1' AS JSON) = 1"),
            same("SELECT CAST('false' AS JSON) = FALSE, CAST('false' AS JSON) = 0"),
            same("SELECT CAST('0' AS JSON) = FALSE, CAST('0' AS JSON) = 0"),
            // Both directions, the NULL-safe spelling, and IN.
            same("SELECT TRUE = CAST('true' AS JSON), 1 = CAST('true' AS JSON)"),
            same("SELECT CAST('true' AS JSON) <=> TRUE, CAST('true' AS JSON) <> TRUE"),
            // `<=>` answers whatever it is given, so it is NOT NULL over
            // nullable columns and over two NULLs — the metadata half of the
            // same operator, which was described VAR_STRING and nullable
            // until this group asked.
            same("SELECT id, f <=> t, f <=> NULL FROM B ORDER BY id"),
            same("SELECT NULL <=> NULL, 1 <=> 1"),
            same("SELECT CAST('true' AS JSON) IN (TRUE), CAST('true' AS JSON) IN (1)"),
            // JSON's type order puts a boolean above a number, so an
            // inequality against a document has a direction and not only a
            // truth value.
            same("SELECT CAST('1' AS JSON) < TRUE, CAST('true' AS JSON) < TRUE"),
            // The conversion rather than the comparison.
            same("SELECT CAST(TRUE AS JSON), CAST(FALSE AS JSON), CAST(1 AS JSON)"),
            same("SELECT JSON_TYPE(CAST(TRUE AS JSON)), JSON_TYPE(CAST(1 AS JSON))"),
            same("SELECT JSON_ARRAY(TRUE, FALSE, 1, 0), JSON_OBJECT('a', TRUE)"),
            // Every yes-or-no answer is one, not just the keyword.
            same("SELECT JSON_ARRAY(1 = 1), JSON_ARRAY(1 < 2), JSON_ARRAY(1 <=> 1)"),
            same("SELECT JSON_ARRAY(1 IN (1, 2)), JSON_ARRAY(1 BETWEEN 0 AND 2)"),
            same("SELECT JSON_ARRAY('a' LIKE 'a'), JSON_ARRAY('a' NOT LIKE 'b')"),
            same("SELECT JSON_ARRAY(1 IS NULL), JSON_ARRAY(1 IS NOT NULL), JSON_ARRAY(1 IS TRUE)"),
            same("SELECT JSON_ARRAY(NOT 0), JSON_ARRAY(1 AND 1), JSON_ARRAY(1 OR 0)"),
            same("SELECT JSON_ARRAY(EXISTS(SELECT 1)), JSON_ARRAY(ISNULL(1))"),
            same("SELECT JSON_ARRAY(JSON_CONTAINS('[1]', '1')), JSON_ARRAY(JSON_VALID('1'))"),
            same("SELECT JSON_ARRAY(REGEXP_LIKE('a', 'a'))"),
            // Unwrapped as well as wrapped: `JSON_ARRAY` exercises the value
            // and hides the column code, which is how `REGEXP_LIKE` stayed
            // VAR_STRING here while answering 0 or 1.
            same("SELECT REGEXP_LIKE('a', 'a'), ISNULL(1), JSON_VALID('1'), JSON_CONTAINS('[1]', '1')"),
            same("SELECT CAST('true' AS JSON) = (1 = 1), CAST('true' AS JSON) = (1 AND 1)"),
            // NULL is not false: a three-valued answer stays JSON null.
            same("SELECT JSON_ARRAY(NOT NULL), JSON_ARRAY(TRUE OR NULL), JSON_ARRAY(NULL AND 0)"),
            // The counterweight, so the tag cannot be read as "anything that
            // answers a small number".
            same("SELECT JSON_ARRAY(LOCATE('a', 'ba')), JSON_ARRAY(CHAR_LENGTH('a'))"),
            // It reads as 1 or 0 everywhere else, and the arithmetic stays
            // exact — `TRUE + 0` is a LONGLONG there, and a double would show
            // as `1.0` inside a document while rendering `1` in a column.
            same("SELECT TRUE, FALSE, TRUE = 1, TRUE + 0, -TRUE, TRUE + TRUE"),
            same("SELECT JSON_ARRAY(TRUE + 0), JSON_ARRAY(-TRUE), JSON_ARRAY(TRUE + TRUE)"),
            same("SELECT JSON_ARRAY(TRUE & 1), JSON_ARRAY(TRUE << 1), JSON_ARRAY(TRUE DIV 1)"),
            // `ABS` has an integer fast path of its own where the rest of
            // these build their answer from a double, so it is the one a
            // boolean fell out of — as a float under a LONGLONG code, which
            // the two protocols disagree about.
            same("SELECT ABS(TRUE), JSON_ARRAY(ABS(TRUE)), JSON_ARRAY(ROUND(TRUE))"),
            same("SELECT JSON_ARRAY(FLOOR(TRUE)), JSON_ARRAY(CEIL(TRUE)), JSON_ARRAY(MOD(TRUE, 2))"),
            same("SELECT CAST(TRUE AS CHAR), CAST(TRUE AS SIGNED), CONCAT(TRUE, '')"),
            // The spellings that separate the numeric reading from the
            // collated one.
            same("SELECT '1' = TRUE, '1.0' = TRUE, '01' = TRUE, 'abc' = TRUE, 1.0 = TRUE"),
            // The tag comes off at every boundary — and the list is not the
            // one an ENUM follows, which is why each is asked separately.
            same("SELECT JSON_ARRAY(IF(1, TRUE, FALSE)), JSON_ARRAY(IF(0, 1, TRUE))"),
            same("SELECT JSON_ARRAY(CASE WHEN 1 THEN TRUE ELSE FALSE END)"),
            same("SELECT JSON_ARRAY(CASE WHEN 0 THEN 1 ELSE TRUE END)"),
            same("SELECT JSON_ARRAY(NULLIF(TRUE, 0)), JSON_ARRAY(COALESCE(TRUE))"),
            same("SELECT JSON_ARRAY(COALESCE(NULL, TRUE)), JSON_ARRAY(IFNULL(TRUE, 0))"),
            same("SELECT JSON_ARRAY(GREATEST(TRUE, FALSE)), JSON_ARRAY(LEAST(TRUE, FALSE))"),
            same("SELECT JSON_ARRAY(MIN(TRUE)), JSON_ARRAY(MAX(TRUE)) FROM B"),
            same("SELECT JSON_ARRAY(SUM(TRUE)), JSON_ARRAY(COUNT(TRUE)) FROM B"),
            same("SELECT JSON_ARRAY(x) FROM (SELECT TRUE AS x) d"),
            same("WITH c AS (SELECT TRUE AS x) SELECT JSON_ARRAY(x) FROM c"),
            same("SELECT JSON_ARRAY(@b), CAST('true' AS JSON) = @b"),
            // A window function, positional as well as aggregate. The
            // positional ones read their subject straight out of a row where
            // the aggregates go through the fold, so they were the family
            // that answered two ways. Read back through a scalar subquery —
            // the one wrapper that keeps the tag — so anything still tagged
            // would answer 1 rather than 0.
            same(
                "SELECT CAST('true' AS JSON) = (SELECT FIRST_VALUE(TRUE) OVER () FROM B LIMIT 1),                  CAST('true' AS JSON) = (SELECT LAST_VALUE(TRUE) OVER () FROM B LIMIT 1)",
            ),
            same(
                "SELECT CAST('true' AS JSON) = (SELECT LEAD(TRUE) OVER (ORDER BY id)                  FROM B LIMIT 1),                  CAST('true' AS JSON) = (SELECT LAG(1, 1, TRUE) OVER (ORDER BY id)                  FROM B LIMIT 1),                  CAST('true' AS JSON) = (SELECT MAX(TRUE) OVER () FROM B LIMIT 1)",
            ),
            // A table value constructor is table-shaped too, and the only one
            // whose *column type* the tag reached: it is derived from the
            // first row, and a boolean is not a `Value::Int`. Both spellings
            // are here so the harness compares the two codes as well as the
            // two values.
            same("VALUES ROW(TRUE), ROW(FALSE)"),
            same("VALUES ROW(1), ROW(0)"),
            // A constructor is NOT NULL only if every row's expression is,
            // since they share one column — so this one is nullable where the
            // two above are not.
            same("VALUES ROW(TRUE), ROW(NULL)"),
            same("SELECT column_0 = CAST('true' AS JSON) FROM (VALUES ROW(TRUE)) v"),
            // Reached through a scalar subquery, because a set operation on
            // its own has nothing outside it to read the tag — and the
            // subquery is the one wrapper that keeps it, which is what makes
            // this the case that isolates the operation.
            same("SELECT JSON_ARRAY((SELECT TRUE UNION SELECT TRUE))"),
            same("SELECT (SELECT TRUE UNION SELECT TRUE) = CAST('true' AS JSON)"),
            same("SELECT CAST('true' AS JSON) IN (SELECT TRUE)"),
            // And the counterweights, so the rule is not read as "the tag
            // never survives anything".
            same("SELECT JSON_ARRAY((SELECT TRUE)), JSON_ARRAY((TRUE)), JSON_ARRAY(+TRUE)"),
            same(
                "SELECT (SELECT TRUE) = CAST('true' AS JSON), \
                 (SELECT x FROM (SELECT TRUE AS x) d) = CAST('true' AS JSON)",
            ),
            // The crux: a column holding 1 is the number 1 however it was
            // written, so it does not match a document of `true` where the
            // keyword beside it does.
            same("SELECT id, j = f, j = t, j = n, j = TRUE, j = 1 FROM B ORDER BY id"),
            same("SELECT id, JSON_ARRAY(f), JSON_ARRAY(t), JSON_ARRAY(n) FROM B ORDER BY id"),
            // Assignment reads the keyword as the number it is, which is the
            // one place a column can tell 1 from '1'.
            same("INSERT INTO BE VALUES (TRUE)"),
            same("SELECT v FROM BE ORDER BY v"),
        ],
    },
    Group {
        // MySQL refuses rather than answering when arithmetic over an UNSIGNED
        // operand leaves its range, and this engine held an `i64` and
        // answered: `iu - 100` over an `INT UNSIGNED` holding 7 was -93 where
        // MySQL raises 1690. #0064 had to leave the UNSIGNED flag off a
        // subtraction because of it.
        //
        // The check is per operator and against the *true* value, which is
        // measured and is the opposite of what #0052 concluded for a bitwise
        // chain: `iu - 100 + 200` is refused though 107 would fit, while
        // `iu - -1` and `100 - iu` are answered. So the cases below vary the
        // operand signs rather than only the operators.
        name: "unsigned arithmetic out of range",
        setup: &[
            "CREATE TABLE U (id INT NOT NULL, tu TINYINT UNSIGNED NULL, \
             su SMALLINT UNSIGNED NULL, mu MEDIUMINT UNSIGNED NULL, iu INT UNSIGNED NULL, \
             bu BIGINT UNSIGNED NULL, i INT NULL, b BIGINT NULL, PRIMARY KEY (id))",
            "INSERT INTO U VALUES (1, 7, 7, 7, 7, 7, 7, 7)",
            // The top of the range, which is the half an engine holding an
            // `i64` could not store at all.
            "CREATE TABLE Wide (id INT NOT NULL, bu BIGINT UNSIGNED NOT NULL, \
             iu INT UNSIGNED NOT NULL, PRIMARY KEY (id))",
            "INSERT INTO Wide VALUES (1, 18446744073709551615, 4294967295)",
            // Two rows, so a window function has a frame to reach past.
            "CREATE TABLE Win (id INT NOT NULL, iu INT UNSIGNED NOT NULL, PRIMARY KEY (id))",
            "INSERT INTO Win VALUES (1, 7), (2, 8)",
            // Declared out of order, so an index and a member name are
            // different answers: position 1 is the member `2`.
            "CREATE TABLE Idx (id INT NOT NULL, g ENUM('2','1') NOT NULL, PRIMARY KEY (id))",
            "INSERT INTO Idx VALUES (1, 1 & 1), (2, 1), (3, 2 & 2), (4, 2)",
        ],
        cases: &[
            // Every width refuses, so this is the column's sign and not its
            // size deciding.
            same("SELECT tu - 100 FROM U"),
            same("SELECT su - 100 FROM U"),
            same("SELECT mu - 100 FROM U"),
            same("SELECT iu - 100 FROM U"),
            same("SELECT bu - 100 FROM U"),
            // Wider than subtraction: a negative operand does it to `+` and
            // `*` as well, which is why the rule is about the answer and not
            // about the operator.
            same("SELECT iu + -100 FROM U"),
            same("SELECT iu * -1 FROM U"),
            same("SELECT iu DIV -1 FROM U"),
            // The literal on the left, where the operand that carries the sign
            // is the right one.
            same("SELECT -100 + iu FROM U"),
            same("SELECT -1 * iu FROM U"),
            same("SELECT 0 - iu FROM U"),
            same("SELECT 6 - iu FROM U"),
            same("SELECT -1 - iu FROM U"),
            same("SELECT -8 DIV iu FROM U"),
            // The chain, which is the case that settles where the check goes:
            // MySQL refuses this and names the inner subtraction.
            same("SELECT iu - 100 + 200 FROM U"),
            same("SELECT iu + 100 - 200 FROM U"),
            // And everything that stays inside the range still answers,
            // negative operands included.
            same("SELECT iu - 7, iu - -1, 100 - iu, 7 - iu, iu + -7 FROM U"),
            same("SELECT iu + 1, iu * 2, iu DIV 2, iu * 0 FROM U"),
            // `%` reads only its left operand for the sign, so these two are
            // the same numbers under different flags.
            same("SELECT iu MOD -1, iu MOD -3, -1 MOD iu, -1 DIV iu FROM U"),
            // A sign drops the flag, so the answer goes negative rather than
            // refusing — and `-bu` refuses for the *signed* reason instead.
            same("SELECT -iu, -iu - 100 FROM U"),
            same("SELECT -bu FROM Wide"),
            // The top of the range: storing it, reading it back, and the
            // arithmetic on either side of the edge.
            same("SELECT bu, bu - 1, bu + -1, bu * 1, bu - bu FROM Wide"),
            same("SELECT bu + 1 FROM Wide"),
            same("SELECT bu * 2 FROM Wide"),
            same("SELECT bu - -1 FROM Wide"),
            same("SELECT bu + bu FROM Wide"),
            same("SELECT iu * iu, iu + iu FROM Wide"),
            // Signed overflow is the same code with a different message, and
            // this engine used to wrap through all of them.
            same("SELECT b + 9223372036854775807 FROM U"),
            same("SELECT b * 9223372036854775807 FROM U"),
            same("SELECT 9223372036854775807 + 1"),
            same("SELECT 9223372036854775807 * 2"),
            same("SELECT -9223372036854775808 - 1"),
            // A literal above `i64::MAX` is a BIGINT UNSIGNED in MySQL, so it
            // subtracts in the unsigned domain like a column would.
            same("SELECT 18446744073709551615 - 1"),
            same("SELECT 18446744073709551615 + 1"),
            same("SELECT 0 - 18446744073709551615"),
            // A cast names the sign in the statement itself.
            same("SELECT CAST(i AS UNSIGNED) - 100 FROM U"),
            same("SELECT CAST(7 AS UNSIGNED) - 100"),
            same("SELECT CAST(bu AS SIGNED), CAST(bu AS CHAR) FROM Wide"),
            // A conditional needs *every* candidate to be unsigned, and the
            // pairs below differ only in whether one of them is — which is the
            // same rule the UNSIGNED flag on the wire follows.
            same("SELECT COALESCE(iu, 1) - 100 FROM U"),
            same("SELECT COALESCE(iu, iu) - 100 FROM U"),
            same("SELECT IF(1, iu, 1) - 100 FROM U"),
            same("SELECT IF(1, iu, iu) - 100 FROM U"),
            same("SELECT IFNULL(iu, 1) - 100 FROM U"),
            same("SELECT IFNULL(iu, iu) - 100 FROM U"),
            same("SELECT GREATEST(iu, 1) - 100 FROM U"),
            same("SELECT GREATEST(iu, iu) - 100 FROM U"),
            same("SELECT LEAST(iu, 1) - 100 FROM U"),
            same("SELECT LEAST(iu, iu) - 100 FROM U"),
            same("SELECT (CASE WHEN 1 THEN iu ELSE 1 END) - 100 FROM U"),
            same("SELECT (CASE WHEN 1 THEN iu ELSE iu END) - 100 FROM U"),
            // A NULL candidate does not count against it, and a missing
            // `ELSE` is a NULL candidate.
            same("SELECT COALESCE(NULL, iu) - 100 FROM U"),
            same("SELECT IF(1, iu, NULL) - 100 FROM U"),
            same("SELECT (CASE WHEN 1 THEN iu END) - 100 FROM U"),
            same("SELECT (CASE WHEN 0 THEN 1 ELSE iu END) - 100 FROM U"),
            // `NULLIF` answers its first argument and takes its sign from
            // that alone, which is why it is not in the list above.
            same("SELECT NULLIF(iu, 1) - 100 FROM U"),
            // The `ELSE` as the *only* unsigned candidate, which is the shape
            // that reads it and nothing else does: the three below are one
            // expression written three ways and MySQL calls all three
            // UNSIGNED. The value has to agree with the flag or the text
            // protocol writes every digit while a binary client reads -1.
            same("SELECT CASE WHEN 0 THEN NULL ELSE bu END FROM Wide"),
            same("SELECT COALESCE(NULL, bu) FROM Wide"),
            same("SELECT IF(0, NULL, bu) FROM Wide"),
            same("SELECT CASE WHEN 0 THEN NULL ELSE iu END - 100 FROM U"),
            // A scalar subquery and a derived table both keep it, where a
            // boolean survives only one of the two and an ENUM only the other.
            same("SELECT (SELECT iu FROM U) - 100"),
            same("SELECT x - 100 FROM (SELECT iu AS x FROM U) t"),
            same("SELECT x - 100 FROM (SELECT bu AS x FROM Wide) t"),
            // A set operation keeps it only where both branches have it, so
            // these two differ by which column the second branch reads.
            same("SELECT x - 100 FROM (SELECT iu AS x FROM U UNION ALL SELECT iu FROM U) t"),
            same("SELECT x - 100 FROM (SELECT iu AS x FROM U UNION ALL SELECT i FROM U) t"),
            same("SELECT iu AS x FROM U UNION SELECT iu FROM U"),
            same("SELECT iu AS x FROM U UNION SELECT i FROM U"),
            // `ABS`, `MIN` and `MAX` hand a row's own value back; `SUM`, `AVG`
            // and `COUNT` build a new number and come back signed.
            same("SELECT ABS(iu) - 100 FROM U"),
            same("SELECT MIN(iu) - 100 FROM U"),
            same("SELECT MAX(iu) - 100 FROM U"),
            same("SELECT SUM(iu) - 100, AVG(iu) - 100, COUNT(iu) - 100 FROM U"),
            same("SELECT SUM(bu), AVG(bu), MIN(bu), MAX(bu) FROM Wide"),
            // A non-integer operand takes the whole thing out of the integer
            // domain, so this answers rather than refusing.
            //
            // Only `100.5` here, and not the `100.0` or the `iu / -1` that
            // were measured beside it: MySQL types both of those NEWDECIMAL
            // and renders `-93.0` and `-7.0000`, where this engine reads a
            // decimal literal as a double and divides into one. That gap
            // predates #0069 and belongs to whichever issue takes decimal
            // literals; putting it here would cost the empty baseline for a
            // reason that has nothing to do with the sign.
            same("SELECT iu - 100.5 FROM U"),
            same("SELECT iu - '100' FROM U"),
            same("SELECT iu - CAST('100' AS SIGNED) FROM U"),
            // Comparison at the top of the range, which the `f64` reading
            // could not tell apart: every `u64` above 2^53 rounds.
            same("SELECT bu = 18446744073709551615, bu = 18446744073709551614 FROM Wide"),
            same("SELECT bu > 0, bu > -1, bu > 9223372036854775807 FROM Wide"),
            same("SELECT bu + 0, CONCAT(bu), JSON_ARRAY(bu) FROM Wide"),
            // A table value constructor gives the value a column of its own,
            // and that column has to be described as the integer it sends: a
            // VAR_STRING code over eight raw bytes desynchronises the row it
            // is in the middle of.
            same("VALUES ROW(18446744073709551615)"),
            same("VALUES ROW(~0)"),
            same("VALUES ROW(1 << 63)"),
            same("VALUES ROW(1)"),
            // Two rows sharing one column, where there is no integer holding
            // both — the same place a mixed-sign union ends up.
            same("VALUES ROW(1), ROW(18446744073709551615)"),
            same("VALUES ROW(18446744073709551615), ROW(18446744073709551614)"),
            // A NULL row says nothing about the column's type, which is the
            // half a fold has to be told.
            same("VALUES ROW(18446744073709551615), ROW(NULL)"),
            // A number is an index into an ENUM whatever its signedness. Read
            // as text, `1 & 1` matched the *member* `1` while the literal 1
            // beside it stored the first member — the same number stored two
            // ways.
            same("SELECT g FROM Idx ORDER BY id"),
            same("SELECT g + 0 FROM Idx ORDER BY id"),
            // These round the number they were given rather than building a
            // new one, so the sign survives — and the negative scale is here
            // because it is the one that cannot short-circuit.
            same("SELECT ROUND(iu) - 100 FROM U"),
            same("SELECT CEIL(iu) - 100 FROM U"),
            same("SELECT FLOOR(iu) - 100 FROM U"),
            same("SELECT TRUNCATE(iu, 0) - 100 FROM U"),
            same("SELECT ROUND(iu, -1) - 100 FROM U"),
            same("SELECT TRUNCATE(iu, -1) - 100 FROM U"),
            same("SELECT ROUND(bu), CEIL(bu), FLOOR(bu), TRUNCATE(bu, 0) FROM Wide"),
            // A negative scale at the top of the range, where the two part
            // company: cutting can only go down, rounding can carry out of it.
            same("SELECT TRUNCATE(bu, -1), TRUNCATE(bu, -2), TRUNCATE(bu, -30) FROM Wide"),
            same("SELECT ROUND(bu, -1) FROM Wide"),
            same("SELECT ROUND(bu - 10, -1), ROUND(bu - 6, -1) FROM Wide"),
            same("SELECT TRUNCATE(bu, -39), ROUND(bu, -39), TRUNCATE(bu, -100) FROM Wide"),
            // A conditional whose candidates are a `BIGINT UNSIGNED` and a
            // signed one has no integer left to widen into, so MySQL answers
            // a decimal and keeps every digit of both.
            same(
                "SELECT COALESCE(Wide.bu, U.b), IFNULL(Wide.bu, U.b), \
                 GREATEST(Wide.bu, U.b) FROM Wide, U",
            ),
            same(
                "SELECT IF(1, Wide.bu, U.b), CASE WHEN 1 THEN Wide.bu ELSE U.b END \
                 FROM Wide, U",
            ),
            same(
                "SELECT COALESCE(Wide.iu, U.i), COALESCE(U.tu, U.i), \
                 COALESCE(Wide.iu, U.b) FROM Wide, U",
            ),
            // Three candidates, which is the only shape that reads the sign
            // carried between them rather than two declarations at once.
            same("SELECT COALESCE(U.i, U.iu, Wide.bu), COALESCE(U.i, U.iu) FROM Wide, U"),
            // `MOD` spelled as a call reads its dividend, exactly as `%` does.
            same("SELECT MOD(iu, -1), MOD(-1, iu) FROM U"),
            // A window function answers a row of its frame, or the default it
            // was given when it reaches past one — and the default decides the
            // sign along with the subject, where the *offset* does not.
            same("SELECT id, LAG(iu) OVER (ORDER BY id) FROM Win ORDER BY id"),
            same("SELECT id, LAG(iu, 1) OVER (ORDER BY id) FROM Win ORDER BY id"),
            same("SELECT id, LAG(iu, 1, 0) OVER (ORDER BY id) FROM Win ORDER BY id"),
            same("SELECT id, FIRST_VALUE(iu) OVER (ORDER BY id) FROM Win ORDER BY id"),
            // Reached through a derived table, because a window function
            // inside an expression is a separate gap in this engine — it is
            // unsupported there whatever the sign, so asking about it here
            // would report that gap rather than this one.
            //
            // `FIRST_VALUE` rather than the `LAG(iu)` measured beside it, and
            // for a reason that is not about the sign: MySQL refuses per row,
            // so `LAG(iu)` sends the first row — whose value is NULL — and
            // *then* raises 1690, where this engine buffers the result and
            // refuses before sending anything. Every row of `FIRST_VALUE`
            // overflows, so the two servers refuse in the same place.
            same(
                "SELECT x - 100 FROM (SELECT id, LAG(iu, 1, 0) OVER (ORDER BY id) AS x \
                 FROM Win) t ORDER BY x",
            ),
            same(
                "SELECT x - 100 FROM (SELECT id, FIRST_VALUE(iu) OVER (ORDER BY id) AS x \
                 FROM Win) t ORDER BY x",
            ),
        ],
    },
    Group {
        // `NO_UNSIGNED_SUBTRACTION` takes `-` out of the unsigned domain and
        // takes nothing else out of it, which is measured rather than read off
        // the flag's name: `iu + -100` and `iu * -1` still refuse under it,
        // and `iu + 1` is still UNSIGNED.
        name: "sql_mode: NO_UNSIGNED_SUBTRACTION",
        setup: &[
            "SET SESSION sql_mode = CONCAT(@@sql_mode, ',NO_UNSIGNED_SUBTRACTION')",
            "CREATE TABLE N (id INT NOT NULL, iu INT UNSIGNED NULL, PRIMARY KEY (id))",
            "INSERT INTO N VALUES (1, 7)",
            "CREATE TABLE NWide (id INT NOT NULL, bu BIGINT UNSIGNED NOT NULL, PRIMARY KEY (id))",
            "INSERT INTO NWide VALUES (1, 18446744073709551615)",
        ],
        cases: &[
            // The whole of the flag: the same subtraction, answered rather
            // than refused, and reported signed rather than UNSIGNED.
            same("SELECT iu - 100, iu - 8, 6 - iu FROM N"),
            // And what it does not touch.
            same("SELECT iu + 1, iu * 2, iu DIV 2 FROM N"),
            same("SELECT iu + -100 FROM N"),
            same("SELECT iu * -1 FROM N"),
            same("SELECT iu DIV -1 FROM N"),
            // A subtraction that leaves the *signed* range now, which is the
            // other half of the flag: it moves the domain rather than removing
            // the check.
            same("SELECT bu - 1 FROM NWide"),
            same("SELECT bu + 1 FROM NWide"),
        ],
    },
    Group {
        // #0056. The operator is four lines of three-valued logic; where it
        // binds is the rest of it. MySQL puts `XOR` between `OR` and `AND`
        // and sqlparser puts it between `+` and `&`, so nearly everything it
        // can be written beside groups the wrong way round. `AND`, `OR` and
        // the comparisons are in here too because a chain stops at an
        // operator with no measured level, so `XOR` cannot be moved past what
        // is not in it — and `||` meaning OR was already being read at the
        // precedence of `*`.
        name: "xor and the loosest precedences",
        setup: &[
            "CREATE TABLE XO (id INT NOT NULL, a INT NOT NULL, b INT NULL, \
             u INT UNSIGNED NOT NULL, s VARCHAR(10) NULL, PRIMARY KEY (id))",
            "INSERT INTO XO VALUES (1, 1, NULL, 3, 'a'), (2, 0, 1, 0, 'b')",
        ],
        cases: &[
            // The truth table, and the three-valued half of it: one unknown
            // operand is enough, where `0 AND NULL` and `1 OR NULL` are not.
            same("SELECT 1 XOR 0, 0 XOR 1, 0 XOR 0, 1 XOR 1"),
            same("SELECT NULL XOR 1, 1 XOR NULL, NULL XOR 0, 0 XOR NULL, NULL XOR NULL"),
            // Truth values and not bits: `5 XOR 3` is 0 where `5 ^ 3` is 6.
            same("SELECT 2 XOR 0, 0 XOR 2, 5 XOR 3, 2.5 XOR 0, 0.0 XOR 1"),
            same("SELECT 'abc' XOR 1, 'abc' XOR 0, '1x' XOR 0"),
            same("SELECT 1 XOR 0 XOR 1, 1 XOR 1 XOR 1, 1 XOR 0 XOR 0 XOR 0"),
            // A SQL boolean where a document can tell one from the number 1.
            same("SELECT JSON_ARRAY(1 XOR 0), JSON_ARRAY(0 XOR 0), JSON_ARRAY(NULL XOR 1)"),
            same("SELECT CAST('true' AS JSON) = (1 XOR 0)"),
            // And an integer everywhere else, which is where a boolean tag
            // has fallen out into a float under a LONGLONG code before.
            same("SELECT -(1 XOR 0), (1 XOR 0) + 1, ABS(1 XOR 0), CONCAT(1 XOR 0)"),
            same("SELECT SUM(1 XOR 0), MAX(1 XOR 0)"),
            // The column metadata: a LONGLONG, NOT NULL only when neither
            // operand can be, never UNSIGNED however unsigned an operand is,
            // and named after the text it was written as.
            same("SELECT id, a XOR a, a XOR b, u XOR a FROM XO ORDER BY id"),
            same("SELECT 1 XOR 0, NULL XOR 1"),
            same("SELECT id, b XOR 1 IS NULL, a XOR 1 IS NULL FROM XO ORDER BY id"),
            same("SELECT id FROM XO WHERE a XOR b IS NULL ORDER BY id"),
            same("SELECT id FROM XO WHERE s = 'a' XOR a = 0 ORDER BY id"),
            // AND binds tighter and OR looser. The parsed shape reads
            // `(2 XOR 3) AND 0`, which is 0.
            same("SELECT 2 XOR 3 AND 0, 0 XOR 1 AND 0, 1 AND 1 XOR 1, 0 AND 1 XOR 1"),
            same("SELECT 1 OR 1 XOR 1, 1 XOR 1 OR 1, 0 AND 1 XOR 1 OR 0"),
            same("SELECT 1 XOR (1 OR 0), (1 XOR 1) OR 1"),
            // Every comparison binds tighter, on either side of the operator.
            same("SELECT 0 XOR 2 = 2, 0 XOR 2 > 1, 1 XOR 2 = 2, 1 = 2 XOR 3"),
            same("SELECT 1 XOR 2 <=> 2, 0 XOR 2 <> 2"),
            // So does every bitwise operator, and so does arithmetic — which
            // is the half sqlparser already had right.
            same("SELECT 1 XOR 2 | 0, 1 XOR 2 & 3, 1 XOR 3 ^ 3, 0 XOR 1 << 1"),
            same("SELECT 1 XOR 2 - 2, 1 XOR 2 * 0, 1 XOR 7 DIV 2, 7 DIV 2 XOR 0"),
            // `NOT` binds tighter than XOR in MySQL and looser in sqlparser,
            // so the whole XOR chain arrives inside it. On two terms that is
            // invisible — `NOT a XOR b` and `NOT (a XOR b)` are the same
            // function — so these two are the control.
            same("SELECT NOT 0 XOR 0, 1 XOR NOT 0"),
            // And these are the claim: the equivalence stops as soon as
            // anything is left outside the NOT.
            same("SELECT NOT 0 XOR 1 AND 0, NOT 0 XOR 0 AND 0, NOT 1 XOR 1 AND 1"),
            same("SELECT NOT NULL XOR 1 AND 0, NOT 0 XOR 1 XOR 1, NOT 1 XOR 0 XOR 1"),
            same("SELECT NOT 0 XOR 1 OR 0, NOT 1 AND 0 XOR 1, NOT NOT 0 XOR 1 AND 0"),
            same("SELECT NOT 0 XOR 2 IN (2), NOT 3 XOR 1 LIKE '1', NOT 0 XOR 1 | 0"),
            // Everything tighter than NOT stays inside it, which is why this
            // is a sink and not the walk `~` takes.
            same("SELECT NOT 0 = 0, NOT 1 + 2, NOT 0 | 1, NOT 3 & 1, NOT 2 IS NULL"),
            same("SELECT NOT (0 XOR 1) AND 0"),
            // The chain the NOT holds need not be at the root of what it
            // holds: `NOT ((2 || NULL) IN (1))` is what arrives.
            same("SELECT NOT 2 || NULL IN (1), NOT NOT 1 IS NULL || NULL"),
            same("SELECT JSON_ARRAY(NOT 0 XOR 1 AND 0)"),
            // `||` means OR under the default mode, and the token is read at
            // the precedence of `*` whichever it means.
            same("SELECT 1 || 1 AND 0, 1 OR 1 AND 0"),
            same("SELECT 1 || 0 XOR 1, 1 XOR 1 || 1, 1 || 1 XOR 1"),
            // The constructs that are not operators but bind where a
            // comparison binds. The parsed shape has each of them at the root
            // with the XOR inside it, which is the wrong way round.
            same("SELECT 1 XOR 2 IS NULL, 1 XOR 0 IS NULL, 1 XOR 2 IS NOT NULL"),
            same("SELECT 1 XOR 2 IS TRUE, 0 XOR 2 IN (2), 1 XOR 2 NOT IN (2)"),
            same("SELECT 0 XOR 2 BETWEEN 1 AND 3, 0 XOR 'a' LIKE 'a', 0 XOR 'a' REGEXP 'a'"),
            // And the operand after the keyword swallowing one instead, which
            // is the same problem `DIV` has. `'a' LIKE 'a' = 1` needs no XOR
            // to go wrong and was wrong before this.
            same("SELECT 'a' LIKE 'a' XOR 0, 'a' REGEXP 'a' XOR 0, 1 BETWEEN 0 AND 2 XOR 1"),
            same("SELECT 2 BETWEEN 0 AND 3 XOR 0, 2 BETWEEN 0 AND 3 XOR 1"),
            same("SELECT 'a' LIKE 'a' = 1"),
            // The construct has to rejoin the chain rather than only come out
            // of it: the AND belongs outside the LIKE and inside the XOR.
            same("SELECT 1 AND 'a' LIKE 'a' XOR 0, 1 XOR 2 IS NULL AND 3"),
            same("SELECT 0 AND 'a' LIKE 'a' XOR 1, 1 XOR 2 IS NULL AND 0"),
            same("SELECT 1 XOR 2 | 3 IS NULL"),
            // The other side of the same level: a comparison ties with the
            // construct and stays outside it.
            same("SELECT 0 = 0 IS NULL, 0 = 0 IS TRUE, 1 | 0 IS NULL"),
            // And `IN`, `BETWEEN`, `LIKE` and `REGEXP` a level tighter than
            // that, which is a `predicate` in MySQL's grammar: this is
            // `2 = (2 IN (1))` and not `(2 = 2) IN (1)`.
            same("SELECT 2 = 2 IN (1), 2 = 2 LIKE 1, 2 = 2 REGEXP '1', 3 = 3 BETWEEN 0 AND 1"),
            // One of these holding another.
            same("SELECT 1 XOR 2 IS NULL IS NULL, 1 XOR 2 IS NULL IS NOT NULL"),
            same("SELECT 0 XOR 2 IS NULL IS NULL, 1 XOR 2 IN (2) IS NULL, 1 XOR 2 IS NULL = 1"),
            // A `LIKE` pattern is the one trailing operand that can swallow
            // another construct — MySQL reads it as a `simple_expr`, where
            // sqlparser reads it below `=`, `IN` and `BETWEEN`.
            same("SELECT 3 LIKE '2' = 3 IN (1, 1), 0 LIKE '2' <> 2 IN (2, 1), 2 LIKE 1 = 1"),
            // And what the walk out of it gives back has to be regrouped: it
            // lifts the leftmost operator and MySQL groups by the loosest.
            same("SELECT 1 LIKE '2' = 0 XOR 1 IS NULL, 3 LIKE '3' <> 3 XOR NULL IS NOT NULL"),
            same("SELECT 0 + NULL LIKE '2' = 1 XOR 3 IS NULL"),
            same("SELECT 1 LIKE '3' XOR 0 + 3 IN (3, 2), 2 | 0 LIKE 'NULL' XOR 3 IN (3, 3)"),
        ],
    },
    Group {
        // The compound spelling, which is the one route to REAL_AS_FLOAT that
        // does not name it. `ANSI` is set on its own rather than added to the
        // mode in force, because that is what it means: MySQL replaces the
        // whole mode with the five flags it expands to.
        name: "sql_mode: ANSI",
        setup: &[
            "SET SESSION sql_mode = 'ANSI'",
            "CREATE TABLE A (id INT NOT NULL, r REAL NULL, dp DOUBLE PRECISION NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO A VALUES (1, 1.234567890123, 1.234567890123)",
        ],
        cases: &[
            // The value and its type code together, which is the whole claim:
            // `ANSI` reached `REAL_AS_FLOAT` without naming it.
            //
            // Deliberately no `information_schema` case here, and no
            // `'a' || 'b'`. Both diverge, and neither divergence is about this
            // mode: MySQL describes information_schema's own columns NOT NULL
            // unless STRICT_TRANS_TABLES is set — which `ANSI` does not set —
            // and it calls a concatenation of two literals NOT NULL where this
            // engine calls it nullable. Adding either would put a second
            // issue's gap in this group and cost the empty baseline.
            same("SELECT r, dp FROM A"),
        ],
    },
    Group {
        name: "extract",
        setup: &[
            "CREATE TABLE Ex (id INT NOT NULL, d DATE NULL, dt DATETIME NULL, \
             dt6 DATETIME(6) NULL, tm TIME NULL, tmneg TIME NULL, tm6 TIME(6) NULL, \
             ts TIMESTAMP NULL, s VARCHAR(40) NULL, fixed DATE NOT NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO Ex VALUES (1, '2024-01-05', '2024-01-05 06:07:08', \
             '2024-01-05 06:07:08.123456', '06:07:08', '-06:07:08', '-06:07:08.123456', \
             '2024-01-05 06:07:08', '2024-01-05 06:07:08', '2024-01-05')",
            // The second row is all NULL bar the two columns that cannot be,
            // so every case below also says what each unit does with one.
            "INSERT INTO Ex (id, fixed) VALUES (2, '2024-01-05')",
        ],
        cases: &[
            // The five units MySQL reads by converting the argument to a
            // DATETIME, and the four it reads by converting it to a TIME. The
            // split shows in the second line: a DATE has no clock, so every
            // clock unit over one is 0.
            same("SELECT EXTRACT(YEAR FROM d), EXTRACT(QUARTER FROM d), \
                  EXTRACT(MONTH FROM d), EXTRACT(WEEK FROM d), EXTRACT(DAY FROM d) \
                  FROM Ex ORDER BY id"),
            same("SELECT EXTRACT(HOUR FROM d), EXTRACT(MINUTE FROM d), \
                  EXTRACT(SECOND FROM d), EXTRACT(MICROSECOND FROM d) \
                  FROM Ex ORDER BY id"),
            same("SELECT EXTRACT(HOUR FROM dt), EXTRACT(MINUTE FROM dt), \
                  EXTRACT(SECOND FROM dt), EXTRACT(MICROSECOND FROM dt) \
                  FROM Ex ORDER BY id"),
            // The fraction, which only a column declared with one carries.
            same("SELECT EXTRACT(MICROSECOND FROM dt6), EXTRACT(SECOND FROM dt6), \
                  EXTRACT(DAY FROM dt6) FROM Ex ORDER BY id"),
            same("SELECT EXTRACT(YEAR FROM ts), EXTRACT(DAY FROM ts), \
                  EXTRACT(SECOND FROM ts) FROM Ex ORDER BY id"),
            // A TIME is signed, and `EXTRACT` carries the sign where the
            // function spelling of the same field does not: `HOUR(tmneg)` is
            // 6 beside an `EXTRACT` of -6.
            same("SELECT EXTRACT(HOUR FROM tmneg), EXTRACT(MINUTE FROM tmneg), \
                  EXTRACT(SECOND FROM tmneg), HOUR(tmneg) FROM Ex ORDER BY id"),
            same("SELECT EXTRACT(MICROSECOND FROM tm6), EXTRACT(SECOND FROM tm6) \
                  FROM Ex ORDER BY id"),
            // A TIME has no calendar, so a calendar unit puts it on today's
            // date. Compared against each server's own `CURDATE` rather than
            // against a literal: the two run in different zones, and the
            // question here is whether the conversion happens at all.
            same("SELECT EXTRACT(YEAR FROM tm) = YEAR(CURDATE()), \
                  EXTRACT(MONTH FROM tm) = MONTH(CURDATE()), \
                  EXTRACT(DAY FROM tm) = DAY(CURDATE()), \
                  EXTRACT(WEEK FROM tm) = EXTRACT(WEEK FROM CURDATE()) \
                  FROM Ex ORDER BY id"),
            // And a negative TIME runs backwards from midnight, so it lands on
            // yesterday. Against midnight shifted by the same 6h07m08s —
            // 22028 seconds — which makes it a claim about the sign rather
            // than about the hour the run happens at.
            same("SELECT EXTRACT(DAY FROM tmneg) = \
                  DAY(DATE_SUB(CURDATE(), INTERVAL 22028 SECOND)), \
                  EXTRACT(MONTH FROM tmneg) = \
                  MONTH(DATE_SUB(CURDATE(), INTERVAL 22028 SECOND)) \
                  FROM Ex ORDER BY id"),
            // Text spelling a datetime, which is read by the same rules once
            // it parses as one — and a VARCHAR column holding the same.
            same("SELECT EXTRACT(YEAR FROM '2024-01-05 06:07:08.123456'), \
                  EXTRACT(HOUR FROM '2024-01-05 06:07:08.123456'), \
                  EXTRACT(MICROSECOND FROM '2024-01-05 06:07:08.123456')"),
            same("SELECT EXTRACT(YEAR FROM s), EXTRACT(SECOND FROM s) FROM Ex ORDER BY id"),
            // Text spelling a bare TIME, which only the clock units read.
            same("SELECT EXTRACT(HOUR FROM '06:07:08'), EXTRACT(MINUTE FROM '-06:07:08'), \
                  EXTRACT(SECOND FROM '-06:07:08')"),
            // Whitespace at either end changes nothing about which reader
            // MySQL picks. Measured, and the reason the length and the
            // separator have to be looked for in the same trimmed text: read
            // one before trimming and one after, and the padding satisfies the
            // separator test on its own.
            same("SELECT EXTRACT(SECOND FROM ' 2024-01-05 06:07:08'), \
                  EXTRACT(SECOND FROM '2024-01-05 06:07:08 '), \
                  EXTRACT(SECOND FROM '  06:07:08  '), \
                  EXTRACT(YEAR FROM ' 2024-01-05 ')"),
            same("SELECT EXTRACT(YEAR FROM NULL), EXTRACT(SECOND FROM NULL), \
                  EXTRACT(WEEK FROM NULL)"),
            // A temporal *expression* rather than a column: MySQL types
            // `DATE_ADD` a string and still reads it as a date, so its clock
            // fields are 0 rather than the 20 minutes past that the leading
            // `2024` of the same characters would spell.
            same("SELECT EXTRACT(DAY FROM DATE_ADD('2024-01-05', INTERVAL 1 DAY)), \
                  EXTRACT(SECOND FROM DATE_ADD('2024-01-05', INTERVAL 1 DAY)), \
                  EXTRACT(HOUR FROM CAST('2024-01-05 06:07:08' AS DATETIME))"),
            // The dates that separate MySQL's week numbering from a naive one:
            // the first days of January belong to the year before, and the
            // last of December can belong to the year after.
            same("SELECT EXTRACT(WEEK FROM '2024-01-01'), EXTRACT(WEEK FROM '2023-01-01'), \
                  EXTRACT(WEEK FROM '2026-12-31'), EXTRACT(WEEK FROM '2020-12-31'), \
                  EXTRACT(WEEK FROM '2015-01-01'), EXTRACT(WEEK FROM '2000-02-29'), \
                  EXTRACT(WEEK FROM '1901-01-01'), EXTRACT(WEEK FROM '2101-01-01')"),
            // The column metadata: LONGLONG, signed, and nullable even over a
            // NOT NULL column — and named after the text it was written as,
            // odd spacing included.
            same("SELECT EXTRACT(YEAR FROM fixed), EXTRACT( WEEK  FROM fixed) \
                  FROM Ex ORDER BY id"),
            // Where a query actually puts one. The aggregate cases are not
            // decoration: an aggregate inside a node sqlparser gives its own
            // grammar has to be folded out before the expression is evaluated,
            // and until `EXTRACT` was added to that walk
            // `EXTRACT(YEAR FROM MAX(d))` reported `max()` as unsupported
            // where `YEAR(MAX(d))` beside it answered.
            same("SELECT id FROM Ex WHERE EXTRACT(YEAR FROM d) = 2024 ORDER BY id"),
            same("SELECT EXTRACT(YEAR FROM MAX(d)), EXTRACT(MONTH FROM MIN(d)) FROM Ex"),
            same("SELECT SUM(EXTRACT(MONTH FROM d)), COUNT(EXTRACT(YEAR FROM d)) FROM Ex"),
            same("SELECT COUNT(*) FROM Ex HAVING EXTRACT(YEAR FROM MAX(d)) = 2024"),
            same("SELECT id FROM Ex ORDER BY EXTRACT(MONTH FROM d) DESC, id"),
            // And ONLY_FULL_GROUP_BY has to see the column *through* it: this
            // is 1055 on both, and was answered with an arbitrary row here
            // until the same walk learned the variant.
            same("SELECT EXTRACT(YEAR FROM d) FROM Ex GROUP BY s"),
            // Grouped by one. Deliberately not *selecting* the group key:
            // MySQL describes a selected grouped expression by the temporary
            // table it materialises into, and that is narrower than the same
            // expression on its own — MYSQL_TYPE_LONG for a two-digit month
            // where every case above agrees on LONGLONG. Measured, and a gap
            // about grouping rather than about EXTRACT, so asking it here
            // would cost the empty baseline for somebody else's issue.
            same("SELECT COUNT(*) FROM Ex GROUP BY EXTRACT(MONTH FROM d) ORDER BY 1"),
            same("SELECT EXTRACT(YEAR FROM NOW()) > 2000, \
                  EXTRACT(MONTH FROM CURDATE()) BETWEEN 1 AND 12, \
                  EXTRACT(HOUR FROM CURTIME()) BETWEEN 0 AND 23"),
        ],
    },
    Group {
        // `EXTRACT(WEEK ...)` takes no mode argument and reads
        // `default_week_format` instead, so the variable is the only way to
        // ask the other seven questions. 1 is the one an application sets:
        // weeks start on Monday and the partial first week is week 1 rather
        // than week 0, which moves every date below.
        name: "default_week_format: 1",
        setup: &["SET SESSION default_week_format = 1"],
        // No `SELECT @@default_week_format` case: an *overridden* system
        // variable comes back from this engine as a string where MySQL sends
        // an UNSIGNED LONGLONG, which is true of every variable a session sets
        // and is a gap about `SET`, not about the week.
        cases: &[
            same("SELECT EXTRACT(WEEK FROM '2024-01-05'), EXTRACT(WEEK FROM '2023-01-01'), \
                  EXTRACT(WEEK FROM '2026-12-31'), EXTRACT(WEEK FROM '2020-12-31'), \
                  EXTRACT(WEEK FROM '2015-01-01'), EXTRACT(WEEK FROM '2000-02-29'), \
                  EXTRACT(WEEK FROM '1901-01-01'), EXTRACT(WEEK FROM '2101-01-01')"),
        ],
    },
    Group {
        // Out of range, which MySQL clamps at `SET` time rather than refusing:
        // 9 is stored as 7, so this group asks the same question as format 7
        // and the way to get it wrong is to mask the value to three bits
        // instead, which turns 9 into 1 and answers a different week.
        name: "default_week_format: out of range",
        setup: &["SET SESSION default_week_format = 9"],
        cases: &[
            same("SELECT EXTRACT(WEEK FROM '2023-01-01'), EXTRACT(WEEK FROM '2024-01-05'), \
                  EXTRACT(WEEK FROM '2020-12-31'), EXTRACT(WEEK FROM '2015-01-01')"),
            // Below the range clamps the other way, to 0 — and the `SET` is a
            // case rather than setup so the group's own mode survives it.
            same("SET SESSION default_week_format = -1"),
            same(
                "SELECT EXTRACT(WEEK FROM '2023-01-01') AS below, \
                 EXTRACT(WEEK FROM '2024-01-05')",
            ),
            // A value that is not an integer is 1232 on both, a string
            // spelling one included.
            same("SET SESSION default_week_format = 'abc'"),
            same("SET SESSION default_week_format = '3'"),
            same("SET SESSION default_week_format = 3.7"),
            same("SET SESSION default_week_format = NULL"),
            // Above `i64::MAX`, which is a literal anyone can write and which
            // arrives as an unsigned value. Read through a signed `i64` it
            // wraps to -1 and clamps to 0, which is the far end of the range
            // from the 7 MySQL stores.
            same("SET SESSION default_week_format = 18446744073709551615"),
            same(
                "SELECT EXTRACT(WEEK FROM '2023-01-01') AS huge, \
                 EXTRACT(WEEK FROM '2024-01-05')",
            ),
            // And an integer-valued expression is taken as the number it is.
            same("SET SESSION default_week_format = 1 + 1"),
            same(
                "SELECT EXTRACT(WEEK FROM '2023-01-01') AS computed, \
                 EXTRACT(WEEK FROM '2024-01-05')",
            ),
        ],
    },
    Group {
        // Bit 1 is the one that makes the count run 1..53 with no week 0, by
        // letting a January date belong to the previous year's last week and a
        // December date to the next year's first. It is the branch of
        // `calc_week` neither of the two formats above reaches.
        name: "default_week_format: 3",
        setup: &["SET SESSION default_week_format = 3"],
        cases: &[
            same("SELECT EXTRACT(WEEK FROM '2024-01-05'), EXTRACT(WEEK FROM '2023-01-01'), \
                  EXTRACT(WEEK FROM '2026-12-31'), EXTRACT(WEEK FROM '2020-12-31'), \
                  EXTRACT(WEEK FROM '2015-01-01'), EXTRACT(WEEK FROM '2000-02-29'), \
                  EXTRACT(WEEK FROM '1901-01-01'), EXTRACT(WEEK FROM '2101-01-01')"),
        ],
    },
    Group {
        // #0058. `SELECT nope.k FROM S`, where nothing named `nope` is in
        // scope, is refused by MySQL when the statement is *prepared*; this
        // engine described it as one column called `k` and failed only when
        // it ran. The harness records a refusal the same way whichever round
        // trip carried it, so what these cases hold is the code and the
        // SQLSTATE — the prepare-time half is held by
        // `crates/engine/tests/describe.rs`.
        //
        // The second half of the group is the risk: resolving a qualifier at
        // all means refusing something legal if the scope is read too
        // narrowly, and every case below the divider is a place a name is in
        // scope without being in this query's FROM clause.
        name: "a qualifier that is not in scope",
        setup: &[
            "CREATE TABLE S (id INT NOT NULL, k INT NULL, PRIMARY KEY (id))",
            "CREATE TABLE T (id INT NOT NULL, sid INT NULL, label VARCHAR(20) NULL, \
             PRIMARY KEY (id))",
            // Left empty on purpose: a name was resolved once per row, so a
            // table with nothing in it answered `SELECT nope.k FROM Bare`
            // with an empty result set rather than 1054.
            "CREATE TABLE Bare (id INT NOT NULL, k INT NULL, PRIMARY KEY (id))",
            "INSERT INTO S VALUES (1, 10), (2, 20)",
            "INSERT INTO T VALUES (1, 1, 'p'), (2, 2, 'q')",
        ],
        cases: &[
            same("SELECT nope.k FROM S"),
            same("SELECT zz FROM S"),
            same("SELECT S.zz FROM S"),
            // A qualifier naming a table that exists but is not in the FROM
            // clause. Being in the database is not being in scope.
            same("SELECT T.label FROM S"),
            // An alias replaces the table name rather than adding to it.
            same("SELECT S.k FROM S x"),
            same("SELECT UPPER(nope.k) FROM S"),
            same("SELECT COUNT(nope.k) FROM S"),
            // 1054 rather than the 1055 ONLY_FULL_GROUP_BY would give: MySQL
            // resolves names before it applies the grouping rules.
            same("SELECT nope.k, COUNT(*) FROM S GROUP BY k"),
            same("SELECT k FROM S WHERE nope.k = 1"),
            same("SELECT COUNT(*) FROM S WHERE nope.k = 1"),
            // An output alias is not in scope in WHERE.
            same("SELECT k + 1 AS x FROM S WHERE x > 1"),
            same("WITH c AS (SELECT id FROM S) SELECT c.zz FROM c"),
            same("SELECT d.zz FROM (SELECT id FROM S) d"),
            // A wildcard's qualifier is a table rather than a column, so it
            // is 1051 and not 1054. Both used to come back as a result set
            // with no columns in it at all.
            same("SELECT nope.* FROM S"),
            same("SELECT S.* FROM S x"),
            // Back-quoted, which is how a generated client writes every
            // identifier. The quotes are part of the spelling and not of the
            // name, and reading the qualifier off the rendered text instead
            // of off its identifiers refused the legal half of this pair.
            same("SELECT `nope`.`k` FROM S"),
            same("SELECT `nope`.* FROM S"),
            // A qualified `*` is read in the query that wrote it and in
            // nothing else, unlike a named column: `S` is in the outer FROM
            // and still 1051 here.
            same("SELECT S.id FROM S WHERE 1 IN (SELECT S.* FROM T)"),
            same("SELECT S.id FROM S WHERE 1 IN (SELECT nope.* FROM T)"),
            same("SELECT S.id FROM S WHERE S.id = (SELECT S.* FROM T LIMIT 1)"),
            // And a named column in an EXISTS select list is resolved even
            // though a `*` in the same place is not.
            same("SELECT COUNT(*) FROM S WHERE EXISTS (SELECT nope.k FROM T)"),
            // The same statements over a relation that yields no rows, which
            // is where the engine answered instead of refusing.
            same("SELECT nope.k FROM Bare"),
            same("SELECT k FROM Bare WHERE nope.k = 1"),
            same("SELECT COUNT(nope.k) FROM Bare"),
            same("SELECT nope.* FROM Bare"),
            same("SELECT nope.k FROM S WHERE 1 = 0"),
            same("SELECT nope.k FROM S LIMIT 0"),

            // Legal from here down, and the reason this group is worth more
            // than its refusals: each is a scope that does not come from the
            // FROM clause of the query the reference is written in.
            // Compared rather than selected: a scalar subquery projected on
            // its own is described VAR_STRING by this engine where MySQL
            // types it from the subquery's column, which is a divergence of
            // its own and not this one.
            same("SELECT S.k, (SELECT COUNT(*) FROM T WHERE T.sid = S.id) > 0 AS has \
                  FROM S ORDER BY S.id"),
            same("SELECT S.k FROM S WHERE EXISTS (SELECT 1 FROM T t WHERE t.sid = S.id) \
                  ORDER BY S.k"),
            same("SELECT S.k FROM S WHERE S.id IN (SELECT t.sid FROM T t WHERE t.id = S.id) \
                  ORDER BY S.k"),
            same("SELECT S.k FROM S WHERE S.id = (SELECT MAX(t.sid) FROM T t \
                  WHERE t.id = S.id) ORDER BY S.k"),
            same("WITH c AS (SELECT id, k FROM S) SELECT c.k FROM c ORDER BY c.id"),
            same("SELECT d.k FROM (SELECT id, k FROM S) d ORDER BY d.id"),
            same("SELECT x.k FROM S x ORDER BY x.id"),
            // Back-quoted qualifiers, which must keep resolving.
            same("SELECT `S`.`k` FROM S ORDER BY `S`.`id`"),
            same("SELECT `S`.* FROM S ORDER BY S.id"),
            same("SELECT `x`.* FROM S `x` ORDER BY x.id"),
            same("SELECT `differential`.`S`.* FROM S ORDER BY S.id"),
            // EXISTS throws its select list away before expanding any `*` in
            // it, so a qualifier naming nothing at all is legal there —
            // through DISTINCT, an extra item, GROUP BY, a UNION, ORDER BY
            // and a negation. Every one of these answers on 8.3, and a
            // wildcard check that did not know it would refuse them.
            same("SELECT COUNT(*) FROM S WHERE EXISTS (SELECT S.* FROM T)"),
            same("SELECT COUNT(*) FROM S WHERE EXISTS (SELECT nope.* FROM T)"),
            same("SELECT COUNT(*) FROM S WHERE EXISTS (SELECT DISTINCT nope.* FROM T)"),
            same("SELECT COUNT(*) FROM S WHERE EXISTS (SELECT nope.*, T.id FROM T)"),
            same("SELECT COUNT(*) FROM S WHERE EXISTS (SELECT nope.* FROM T GROUP BY T.id)"),
            same("SELECT COUNT(*) FROM S \
                  WHERE EXISTS (SELECT nope.* FROM T UNION SELECT T.* FROM T)"),
            same("SELECT COUNT(*) FROM S WHERE EXISTS (SELECT nope.* FROM T ORDER BY T.id)"),
            // What replaces the `*` is one column, not none, and a UNION
            // compares that arity: 8.3 answers the first and refuses the
            // second with 1222.
            same("SELECT COUNT(*) FROM S \
                  WHERE EXISTS (SELECT nope.* FROM T UNION SELECT T.id FROM T)"),
            same("SELECT COUNT(*) FROM S \
                  WHERE EXISTS (SELECT nope.* FROM T UNION SELECT T.id, T.sid FROM T)"),
            same("SELECT COUNT(*) FROM S WHERE EXISTS (SELECT nope.* FROM T WHERE T.id = 99)"),
            same("SELECT COUNT(*) FROM S \
                  WHERE NOT EXISTS (SELECT nope.* FROM T WHERE T.id = 99)"),
            same("SELECT a1.k, a2.k AS second FROM S a1 JOIN S a2 ON a1.id = a2.id \
                  ORDER BY a1.id"),
            // USING merges the two `id` columns into one, and the qualified
            // form still resolves against it.
            same("SELECT S.id FROM S JOIN T USING (id) ORDER BY S.id"),
            // An output alias *is* in scope in ORDER BY, unlike in WHERE.
            same("SELECT k AS ali FROM S ORDER BY ali"),
            // The database written out in front of the table, on both sides
            // of the FROM.
            same("SELECT S.k FROM differential.S ORDER BY S.id"),
            same("SELECT differential.S.k FROM S ORDER BY S.id"),
            same("SELECT S.* FROM S ORDER BY S.id"),
            same("SELECT x.* FROM S x ORDER BY x.id"),
            // A bare keyword that parses as an identifier and is not a
            // column, in a statement that also reads one.
            same("SELECT TIMESTAMPDIFF(DAY, '2026-01-01', '2026-03-05') AS d, S.k \
                  FROM S ORDER BY S.id"),
            // A system variable, in the spelling that parses as a qualified
            // name. Compared as a predicate rather than as itself, because
            // the two servers are different versions.
            same("SELECT @@GLOBAL.version IS NOT NULL AS v, S.k FROM S ORDER BY S.id"),
        ],
    },
];
