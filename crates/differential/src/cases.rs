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
            // Only answers up to `i64::MAX` are here: the engine holds an i64
            // and refuses what it cannot represent, which is its own gap and
            // not something this file can ask about.
            same("SELECT -1 & 1, -1 >> 1, ~-6, ~-1 & 255, 1 + 3 & ~-6"),
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
];
