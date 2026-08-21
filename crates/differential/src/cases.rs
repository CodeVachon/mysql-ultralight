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
             bu BIGINT UNSIGNED, y YEAR)",
            "INSERT INTO Q VALUES (100, 30000, 2000000000, 4000000000, 9000000000, 2026)",
            "INSERT INTO Q VALUES (100, 30000, 2000000000, 4000000000, 9000000000, 1901)",
            "INSERT INTO Q VALUES (100, 30000, 2000000000, 4000000000, 9000000000, 2155)",
        ],
        cases: &[
            // A total outgrows what it totals: three TINYINTs of 100 make 300,
            // and announcing the column as one byte sends 44.
            same("SELECT SUM(t), SUM(s), SUM(i) FROM Q"),
            same("SELECT AVG(t), MIN(t), MAX(t), COUNT(t) FROM Q"),
            // An unsigned column read back signed comes out negative.
            same("SELECT iu, bu FROM Q LIMIT 1"),
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
                 at DATETIME(3) NULL,
                 d DATE NULL,
                 kind ENUM('x','y') NULL,
                 flag BOOLEAN NULL,
                 PRIMARY KEY (id))",
            "INSERT INTO T (small, big, name, note, amt, approx, at, d, kind, flag) \
             VALUES (5, 9000000000, 'n', 'note', '1.25', 2.5, '2026-01-02 03:04:05.678', \
             '2026-01-02', 'x', 1)",
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
            // A function this engine does not implement has to say so, even
            // when the rule above would otherwise answer for it.
            same("SELECT SOUNDEX(NULL)"),
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
            same("SELECT COALESCE(t, 'none'), IFNULL(n, 0), NULLIF(n, 5) FROM S ORDER BY id"),
            same("SELECT ROUND(n / 3, 2), ABS(n), n DIV 2, n % 3 FROM S ORDER BY id"),
            same("SELECT t LIKE 'H%', t REGEXP '^H', t IS NULL FROM S ORDER BY id"),
            same("SELECT CAST(n AS CHAR), CAST(t AS UNSIGNED) FROM S ORDER BY id"),
            same("SELECT id FROM S WHERE n > 0 ORDER BY id"),
            same("SELECT id FROM S WHERE t = 'hello' ORDER BY id"),
            same("SELECT MD5('x'), LEFT(t, 2), RIGHT(t, 2) FROM S ORDER BY id"),
        ],
    },
];
