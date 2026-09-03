//! The SQL both servers are asked.
//!
//! Seeded from the places this engine has actually been wrong. Every entry
//! under "answers I got wrong" is a bug a reviewer caught by knowing MySQL;
//! having them here means the next one is caught by asking it.

pub struct Case {
    pub sql: &'static str,
    /// Whether the widths of this case's columns are compared.
    ///
    /// True for all but a handful. MySQL takes a column's `column_length` from
    /// its declaration, and takes the declaration of a view's or a
    /// `CREATE TABLE ... AS SELECT`'s column from `Item::max_length` — the
    /// width the expression behind it can produce. This engine has no such
    /// arithmetic and declares `varchar(512)` for any computed string, so a
    /// view over `'ab'` is `varchar(512)` here against `varchar(2)` on 8.3.
    ///
    /// The cases that hit it are kept and the width alone is not asked, rather
    /// than the cases being cut: what they are *for* — that a view re-derives
    /// the name MySQL gives a literal — is answered either way, and cutting
    /// them to keep a comparison green would trade a real answer for a
    /// cosmetic one. Every one of them carries the reason above it, and the
    /// list only ever gets shorter.
    pub widths: bool,
    /// Whether this case's columns are asked to agree on bytes against
    /// characters.
    ///
    /// True for all but eleven. MySQL decides it from the collation of what
    /// produced the column, and this engine has no collation to propagate: a
    /// string function over a NULL literal or over an explicitly `BINARY`
    /// value is binary there and text here. The other direction is MySQL's own
    /// quirk — a temporal that has been through a temporary table comes back
    /// in the connection's character set, where the same column read straight
    /// from its table is binary.
    ///
    /// Worth asking of everything else, and asked of computed columns where
    /// nothing else in the definition is: a column of characters described as
    /// bytes reaches the caller as a Buffer it cannot convert to a string, and
    /// this is the only line in `compare.rs` that would catch one.
    pub value_kind: bool,
}

impl Case {
    /// Stops asking about the widths of this case's columns. See
    /// `Case::widths`, and give every use a reason of its own.
    const fn without_widths(mut self) -> Case {
        self.widths = false;
        self
    }

    /// Stops asking whether this case's columns are bytes or characters. See
    /// `Case::value_kind`, and give every use a reason of its own.
    const fn without_value_kind(mut self) -> Case {
        self.value_kind = false;
        self
    }
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
    Case {
        sql,
        widths: true,
        value_kind: true,
    }
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
            // A decimal inside a document keeps its scale, which is a thing
            // the corpus had no case for at all — every number in it was an
            // integer or a double. #0071.
            "CREATE TABLE Money (id INT NOT NULL, a DECIMAL(10,2) NULL, b DECIMAL(10,4) NULL, \
             c DECIMAL(10,0) NULL, w DECIMAL(30,2) NULL, PRIMARY KEY (id))",
            "INSERT INTO Money VALUES (1, '2.50', '2.5000', '2', '123456789012345678901.00'), \
             (2, '-3.10', '-3.1000', '-3', '123456789012345678902.00'), \
             (3, '0.00', '0.0000', '0', '0.00')",
            // Two ways into a JSON column, which are two different parses:
            // the constructor's document keeps the decimal, the string is
            // source text and its `2.50` is the double 2.5.
            "CREATE TABLE MoneyJ (id INT NOT NULL, doc JSON NULL, PRIMARY KEY (id))",
            "INSERT INTO MoneyJ SELECT id, JSON_OBJECT('a', a) FROM Money",
            "INSERT INTO MoneyJ VALUES (9, '{\"a\": 2.50}')",
            // `$serde_json::private::Number` is how serde_json passes a
            // number it holds as text through a visitor, and an ordinary key
            // to MySQL. Row 4 is built by a constructor rather than written
            // as text, and row 7 holds that key beside a real number.
            "CREATE TABLE Sentinel (id INT NOT NULL, doc JSON NULL, PRIMARY KEY (id))",
            "INSERT INTO Sentinel VALUES \
             (1, '{\"$serde_json::private::Number\": \"1\"}'), \
             (2, '{\"$serde_json::private::Number\": 1}'), \
             (3, '{\"$serde_json::private::Number\": \"abc\"}'), \
             (5, '{\"$serde_json::private::Number\": \"1\", \"b\": 2}'), \
             (6, '{\"b\": 2, \"$serde_json::private::Number\": \"1\"}'), \
             (7, '[2.50, {\"$serde_json::private::Number\": \"1\"}]')",
            "INSERT INTO Sentinel VALUES (4, JSON_OBJECT('$serde_json::private::Number', 1))",
            "CREATE TABLE Approx (f FLOAT NULL, d DOUBLE NULL)",
            "INSERT INTO Approx VALUES (1.234567890123, 1.234567890123), (16777217, 16777217)",
            // Written with more digits than a double carries, which is where
            // the JSON parser's shortcuts decide the value. #0072.
            "CREATE TABLE Wide (id INT NOT NULL, doc JSON NULL, PRIMARY KEY (id))",
            "INSERT INTO Wide VALUES (1, '{\"a\": 2000000000000000.25}'), \
             (2, '[12345678901234.5678]'), (3, '{\"a\": -9945313574470124358}')",
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
            // A decimal keeps the digits its scale declares, whichever
            // constructor builds the document — `2.50` and not `2.5`.
            same(
                "SELECT id, JSON_OBJECT('a', a, 'b', b, 'c', c), JSON_ARRAY(a, b, c) \
                 FROM Money ORDER BY id",
            ),
            same("SELECT id, CAST(a AS JSON), JSON_SET('{\"x\": 1}', '$.x', a) FROM Money ORDER BY id"),
            same("SELECT id, JSON_MERGE_PATCH(JSON_OBJECT('a', a), '{\"b\": 2}') FROM Money ORDER BY id"),
            same("SELECT JSON_ARRAYAGG(a), JSON_ARRAY(SUM(a)), JSON_ARRAY(MIN(a)) FROM Money"),
            same("SELECT JSON_UNQUOTE(JSON_EXTRACT(JSON_OBJECT('a', a), '$.a')) FROM Money WHERE id = 1"),
            // Stored and read back. Row 9 arrived as a string rather than as
            // a document, so its `2.50` is the double the text spells.
            same("SELECT id, doc, doc->'$.a', doc->>'$.a' FROM MoneyJ ORDER BY id"),
            same("SELECT id, JSON_EXTRACT(doc, '$.a') = 2.50, JSON_EXTRACT(doc, '$.a') = 2.5 FROM MoneyJ ORDER BY id"),
            // Source text is the other side of that boundary and has no
            // decimal in it: `2.50` is the double 2.5, `-0` the integer 0,
            // and `1E2` is written back as `100.0`.
            same("SELECT CAST('[2.50, 2.0, 2, -0, 1E2]' AS JSON)"),
            // Every spelling of one number is one number, and the key agrees
            // with the comparison.
            same("SELECT CAST('2.50' AS JSON) = CAST('2.5' AS JSON), JSON_OBJECT('a', 2.50) = JSON_OBJECT('a', 2.5)"),
            same("SELECT JSON_CONTAINS(JSON_OBJECT('a', a), '2.5', '$.a'), JSON_CONTAINS('[2.0]', '2'), JSON_CONTAINS('[2]', '2.0') FROM Money WHERE id = 1"),
            // Two decimals one double cannot tell apart are two documents.
            same("SELECT id, JSON_ARRAY(w) FROM Money ORDER BY id"),
            same("SELECT COUNT(DISTINCT JSON_ARRAY(w)) FROM Money"),
            // A number no double can hold is not valid JSON text, which is a
            // refusal the parser has to make now that the digits are kept.
            same("SELECT JSON_VALID('1e2000'), JSON_VALID('[1e2000]'), JSON_VALID('1e308')"),
            // Serde's number sentinel is a key like any other here. Reading
            // it as its private protocol turned row 1 into the number 1,
            // made rows 2 and 3 invalid JSON, and refused row 4 against this
            // engine's own rendering.
            same("SELECT id, doc, JSON_TYPE(doc), JSON_LENGTH(doc) FROM Sentinel ORDER BY id"),
            same(
                "SELECT JSON_VALID('{\"$serde_json::private::Number\": \"1\"}'), \
                 JSON_VALID('{\"$serde_json::private::Number\": 1}'), \
                 JSON_VALID('{\"$serde_json::private::Number\": \"abc\"}')",
            ),
            same(
                "SELECT JSON_EXTRACT(doc, '$.\"$serde_json::private::Number\"') \
                 FROM Sentinel WHERE id = 1",
            ),
            // A cast converts an approximate number rather than reading back
            // the text its column displays, which for a FLOAT is six
            // significant digits. Both routes into a document agree.
            same("SELECT CAST(f AS JSON), JSON_ARRAY(f), CAST(d AS JSON), JSON_ARRAY(d) FROM Approx ORDER BY f"),
            // Which double a literal names, which is MySQL's answer as much
            // as the spelling is. Its parser keeps seventeen significant
            // digits and scales them by one power of ten, so a literal
            // written with more than that lands somewhere a correctly-rounded
            // parse does not: `2000000000000000.25` is exactly a double and
            // is still read as 2e15.
            same("SELECT CAST('[2000000000000000.25, -2000000000000000.25, 2000000000000000.75]' AS JSON)"),
            // Trailing zeros count towards the seventeen, and the first digit
            // does not — so a padded spelling is a different number, and
            // counting the first digit would move all three of these.
            same("SELECT CAST('[6193038.5328500000000, 945177346.838600900, 2963649940394770.84]' AS JSON)"),
            // The exact accumulator stops the moment it passes 2^53 rather
            // than when it runs out of digits, and what it leaves goes
            // through the double.
            same("SELECT CAST('[12345678901234.5678, 99555829226.40225000, 987119.34197208915]' AS JSON)"),
            // A negative hands over to the double at 2^63 where a positive
            // waits for 2^64, so a sign changes the value and not only its
            // spelling.
            same("SELECT CAST('[-9945313574470124358, -116099708312755456469406171708, 123456789012345678901234567890]' AS JSON)"),
            // The scaling divides by the positive power from a table of
            // nearest doubles, which is neither multiplying by the negative
            // one nor computing the power.
            same("SELECT CAST('[442764011e263, 9.63e-143, -408.38500]' AS JSON)"),
            // The exponent is measured against where the point already sits,
            // so `e312` is out of range at one place and 1e307 five places
            // down; below the table the scaling takes two steps.
            same("SELECT CAST('[0.00001e312, 0.00000000000000001e325, 9e-309]' AS JSON)"),
            same("SELECT CAST('[1e-99999999999999999999, 1e-2000, 1e-320]' AS JSON)"),
            // Leading zeros in a fraction are not significant digits.
            same("SELECT CAST('[0.000000000000000000000000021649319015913176468, 0.0000000000000000000944876924289080511]' AS JSON)"),
            // Which of those are numbers at all, which the exponent decides
            // before any arithmetic happens.
            same("SELECT JSON_VALID('1e309'), JSON_VALID('0.00000000000000001e326'), JSON_VALID('1.7976931348623159e308'), JSON_VALID('0.00001e312'), JSON_VALID('0.00001e3120')"),
            // Stored and read back, so the value is the column's and not the
            // expression's.
            same("SELECT id, doc, doc->'$.a', JSON_EXTRACT(doc, '$[0]') FROM Wide ORDER BY id"),
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
        // Everything GROUP_CONCAT reads that no other aggregate does: its own
        // ORDER BY, its SEPARATOR, its DISTINCT and `group_concat_max_len`.
        // All four were parsed and thrown away — #0088 — so the answer was a
        // plausible string in the wrong order, joined with the wrong thing,
        // missing every argument after the first.
        //
        // No two rows tie on a sort key anywhere here. 8.3 breaks a tie by
        // putting the later row first, which is its insert-order tree showing
        // through rather than an order it defines, and this engine sorts
        // stably — so a tied case would be this file asserting a difference
        // nobody should depend on, which is the rule about orders that are not
        // total applied inside a value.
        name: "group_concat clauses",
        setup: &[
            "CREATE TABLE Gcc (id INT NOT NULL, k INT NOT NULL, s VARCHAR(10) NULL, \
             n INT NULL, PRIMARY KEY (id))",
            // Arrival order is `b, a, c`, which is none of the orders the
            // cases ask for.
            "INSERT INTO Gcc VALUES (1,1,'b',3),(2,1,'a',1),(3,1,'c',2),(4,2,'z',9)",
            "CREATE TABLE GccE (id INT NOT NULL, k INT NULL, s VARCHAR(10) NULL, \
             n INT NULL, PRIMARY KEY (id))",
            "CREATE TABLE GccD (id INT NOT NULL, s VARCHAR(10) NULL, n INT NULL, \
             PRIMARY KEY (id))",
            // Two rows share an `s` and not an `n`, which is what tells
            // "DISTINCT keeps the first row" from "DISTINCT keeps the last".
            "INSERT INTO GccD VALUES (1,'a',5),(2,'b',2),(3,'a',1),(4,'c',9)",
            "CREATE TABLE GccN (id INT NOT NULL, k INT NOT NULL, s VARCHAR(10) NULL, \
             n INT NULL, PRIMARY KEY (id))",
            "INSERT INTO GccN VALUES (1,1,'a',NULL),(2,1,'b',7),(3,2,NULL,1),(4,2,NULL,2)",
            // 300 bytes of one character and 300 bytes of another that is two
            // bytes wide, so a limit between them separates a byte budget from
            // a character count and lands where a character does not fit.
            "CREATE TABLE GccU (id INT NOT NULL, s VARCHAR(400) NULL, PRIMARY KEY (id))",
            "INSERT INTO GccU VALUES (1, RPAD('a', 300, 'a')), (2, RPAD('é', 150, 'é'))",
        ],
        cases: &[
            same("SELECT GROUP_CONCAT(s ORDER BY s), GROUP_CONCAT(s ORDER BY n), \
                  GROUP_CONCAT(s ORDER BY s DESC), GROUP_CONCAT(s ORDER BY n DESC, s) \
                  FROM Gcc WHERE k = 1"),
            // A position names an argument of the call, so the second one
            // sorts on `n` — and every argument is concatenated per row.
            same("SELECT GROUP_CONCAT(s ORDER BY 1), GROUP_CONCAT(s, n ORDER BY 2), \
                  GROUP_CONCAT(k, s, n), GROUP_CONCAT(s ORDER BY -n) FROM Gcc WHERE k = 1"),
            same("SELECT k, GROUP_CONCAT(s ORDER BY s DESC) FROM Gcc GROUP BY k ORDER BY k"),
            same("SELECT GROUP_CONCAT(s SEPARATOR ' - '), GROUP_CONCAT(s SEPARATOR ''), \
                  GROUP_CONCAT(s ORDER BY s SEPARATOR '|'), GROUP_CONCAT(s, n SEPARATOR '') \
                  FROM Gcc WHERE k = 1"),
            same("SELECT k FROM Gcc GROUP BY k HAVING GROUP_CONCAT(s ORDER BY s) = 'a,b,c'"),
            same("SELECT GROUP_CONCAT(s ORDER BY x) FROM \
                  (SELECT s, n AS x FROM Gcc WHERE k = 1) t"),
            // DISTINCT sorts what it keeps when nothing else says otherwise,
            // and it sorts by the value rather than by its text.
            same("SELECT GROUP_CONCAT(s), GROUP_CONCAT(DISTINCT s), GROUP_CONCAT(DISTINCT n), \
                  GROUP_CONCAT(DISTINCT s SEPARATOR '-') FROM GccD"),
            // With an ORDER BY it keeps the first row of each value and sorts
            // on that row's key: the surviving `a` sorts on 5, not on 1.
            same("SELECT GROUP_CONCAT(DISTINCT s ORDER BY n), \
                  GROUP_CONCAT(DISTINCT s ORDER BY n DESC), GROUP_CONCAT(s ORDER BY n) \
                  FROM GccD"),
            // A NULL is dropped, an all-NULL group is NULL, and a row with one
            // NULL argument contributes nothing rather than its other
            // arguments.
            same("SELECT k, GROUP_CONCAT(s ORDER BY s) FROM GccN GROUP BY k ORDER BY k"),
            same("SELECT GROUP_CONCAT(s, n) FROM GccN WHERE k = 1"),
            same("SELECT GROUP_CONCAT(s ORDER BY s SEPARATOR '|') FROM GccE"),
            same("SELECT k, GROUP_CONCAT(s) FROM GccE GROUP BY k ORDER BY k"),
            // The refusals, each also asked where nothing is folded — an empty
            // table, and a GROUP BY that makes no groups — because a check
            // living in the fold would not run there at all.
            same("SELECT GROUP_CONCAT(s ORDER BY 2) FROM Gcc"),
            same("SELECT GROUP_CONCAT(s ORDER BY 0) FROM GccE"),
            same("SELECT GROUP_CONCAT(s, n ORDER BY 3) FROM Gcc"),
            same("SELECT GROUP_CONCAT(s ORDER BY zz) FROM GccE"),
            same("SELECT k, GROUP_CONCAT(s ORDER BY 2) FROM GccE GROUP BY k"),
            same("SELECT k FROM Gcc GROUP BY k HAVING GROUP_CONCAT(s ORDER BY zz) = 'x'"),
            // The grammar takes a `text_string` separator, which is wider than
            // a quoted string: a hex literal is a string of bytes.
            same("SELECT GROUP_CONCAT(s SEPARATOR 0x2D), GROUP_CONCAT(s SEPARATOR X'2D'), \
                  GROUP_CONCAT(s SEPARATOR 0x2D2D) FROM Gcc WHERE k = 1"),
            // 8.3 has no GROUP_CONCAT window function at all, and it settles
            // that while resolving the item — so a FROM-less select, an
            // untaken CASE branch, an empty relation and an ORDER BY are each
            // 1235 there, and none of them reaches a window pass here.
            same("SELECT GROUP_CONCAT(s) OVER () FROM Gcc"),
            same("SELECT GROUP_CONCAT(s) OVER (PARTITION BY k) FROM GccE"),
            same("SELECT GROUP_CONCAT('a') OVER ()"),
            same("SELECT CASE WHEN 0 THEN GROUP_CONCAT(s) OVER () ELSE 1 END FROM Gcc"),
            same("SELECT GROUP_CONCAT(s) OVER () FROM Gcc WHERE 1 = 0"),
            same("SELECT 1 FROM Gcc ORDER BY GROUP_CONCAT(s) OVER ()"),
            // And it is the last of the resolve-time refusals: each of these
            // is the other error on 8.3 rather than the 1235.
            same("SELECT GROUP_CONCAT('a' ORDER BY 2) OVER ()"),
            same("SELECT GROUP_CONCAT(zz) OVER () FROM Gcc"),
            // `group_concat_max_len` truncates the answer, in bytes and never
            // through a character: 601 bytes of 451 characters, cut at 600, is
            // 599 bytes of 450 — the last two-byte character did not fit.
            //
            // Asked through LENGTH rather than by comparing a 600-byte string,
            // and every limit here is above 512 on purpose: at or below that
            // 8.3 describes GROUP_CONCAT as a VAR_STRING rather than a
            // LONG_BLOB, and this engine describes it the same way whatever
            // the limit is. That is a gap about the *type*, which nothing here
            // reads, and it needs an issue rather than a case that hides it.
            same("SELECT LENGTH(GROUP_CONCAT(s ORDER BY s)), \
                  CHAR_LENGTH(GROUP_CONCAT(s ORDER BY s)) FROM GccU"),
            // The `SET` is a case rather than setup so the group's own limit
            // survives it, the way the week-format group does it.
            same("SET SESSION group_concat_max_len = 600"),
            same("SELECT LENGTH(GROUP_CONCAT(s ORDER BY s)) AS cut, \
                  CHAR_LENGTH(GROUP_CONCAT(s ORDER BY s)) FROM GccU"),
            same("SET SESSION group_concat_max_len = 601"),
            same("SELECT LENGTH(GROUP_CONCAT(s ORDER BY s)) AS whole, \
                  CHAR_LENGTH(GROUP_CONCAT(s ORDER BY s)) FROM GccU"),
            // A value that is not an integer is 1232 on both, a string
            // spelling one included.
            same("SET SESSION group_concat_max_len = 'abc'"),
            same("SET SESSION group_concat_max_len = '3'"),
            same("SET SESSION group_concat_max_len = 3.7"),
            same("SET SESSION group_concat_max_len = NULL"),
        ],
    },
    Group {
        // What makes a query grouped, and what a HAVING can see. Nothing here
        // asks about aggregation itself; the point is the boundary either side
        // of the clause, which is where #0103 sat.
        name: "having",
        setup: &[
            "CREATE TABLE Hv (id INT NOT NULL, b INT NULL, s VARCHAR(10) NOT NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO Hv VALUES (1,1,'x'),(2,2,'y'),(3,3,'z'),(4,NULL,'w')",
            "CREATE TABLE HvE (id INT NOT NULL, b VARCHAR(10) NULL, PRIMARY KEY (id))",
        ],
        cases: &[
            // A HAVING with no aggregate does not group the query: every
            // column stays available and the clause filters rows. This engine
            // read the clause as grouping and refused all of these with 1055.
            same("SELECT b FROM Hv HAVING b > 1 ORDER BY b"),
            same("SELECT * FROM Hv HAVING b > 1 ORDER BY id"),
            same("SELECT b FROM Hv HAVING b IS NULL"),
            same("SELECT b FROM Hv HAVING 1 ORDER BY b"),
            same("SELECT b FROM Hv HAVING 0"),
            same("SELECT b FROM Hv HAVING NULL"),
            // Settled after the projection, so it reads an output alias, and
            // before DISTINCT, ORDER BY and LIMIT.
            same("SELECT b + 1 AS bp FROM Hv HAVING bp > 2 ORDER BY bp"),
            same("SELECT DISTINCT s FROM Hv WHERE id < 4 HAVING s > 'w' ORDER BY s"),
            same("SELECT b FROM Hv HAVING b > 1 ORDER BY b LIMIT 1"),
            // An aggregate inside a subquery belongs to that subquery, so the
            // outer query is still per-row.
            same("SELECT b FROM Hv HAVING b > (SELECT MIN(b) FROM Hv) ORDER BY b"),
            same(
                "SELECT b FROM Hv HAVING (SELECT COUNT(*) FROM HvE WHERE HvE.id = Hv.id) = 0 \
                 ORDER BY b",
            ),
            // The clause resolves a name by what it is rather than by how it
            // is spelled, and an alias beats a column.
            same("SELECT b AS zzz FROM Hv HAVING b > 1 ORDER BY zzz"),
            same("SELECT b FROM Hv HAVING Hv.b > 1 ORDER BY b"),
            // Ordered by position rather than by the alias, which is a
            // different rule and a divergence of its own: `SELECT id AS b FROM
            // Hv ORDER BY b` sorts by the alias on 8.3 and by the column `b`
            // here, with no HAVING anywhere near it.
            same("SELECT id AS b FROM Hv HAVING b > 1 ORDER BY 1"),
            same("SELECT Hv.b, b FROM Hv HAVING b > 1 ORDER BY 1"),
            same("SELECT id, b AS bb FROM Hv HAVING bb > 1 ORDER BY id"),
            same("SELECT x.b FROM Hv AS x HAVING x.b > 1 ORDER BY 1"),
            // An enclosing query's column is in scope; this query's own FROM
            // is not. Asked from the WHERE clause and from the select list:
            // the second was written this way round only because a projected
            // scalar subquery used to be declared VAR_STRING here (#0104), and
            // it is the shape that reads the subquery's *answer* rather than
            // only whether it has one.
            same(
                "SELECT id FROM Hv WHERE (SELECT MAX(x.b) FROM Hv x HAVING Hv.id > 2) \
                 IS NOT NULL ORDER BY id",
            ),
            same(
                "SELECT id, (SELECT MAX(x.b) FROM Hv x HAVING Hv.id > 2) AS m \
                 FROM Hv ORDER BY id",
            ),
            same("SELECT b FROM Hv HAVING id > 1"),
            same("SELECT COUNT(*) FROM Hv HAVING b > 1"),
            same("SELECT b FROM Hv GROUP BY b HAVING id > 1"),
            same("SELECT b + 1 FROM Hv HAVING b + 1 > 2"),
            same("SELECT UPPER(s) FROM Hv HAVING s > 'w'"),
            same("SELECT SUM(id) FROM Hv HAVING SUM(nope) > 1"),
            same("SELECT SUM(id) FROM Hv HAVING SUM(id) > 1"),
            same("SELECT s, COUNT(*) FROM Hv GROUP BY s HAVING s > 'w' ORDER BY s"),
            // Two things one name could mean.
            same("SELECT id AS b, Hv.b FROM Hv HAVING b > 1"),
            same("SELECT * FROM Hv a JOIN Hv c ON a.id = c.id HAVING id > 1"),
            same("SELECT a.*, c.b FROM Hv a JOIN Hv c ON a.id = c.id HAVING b > 1"),
            same("SELECT a.id FROM Hv a JOIN Hv c ON a.id = c.id HAVING id > 1 ORDER BY 1"),
            same("SELECT * FROM Hv HAVING nope > 1"),
            // An aggregate written in the HAVING and nowhere else *does* group
            // the query, which is 1140 and not 1055 -- a different complaint
            // from a column missing from a GROUP BY that exists.
            same("SELECT b FROM Hv HAVING COUNT(*) > 0"),
            same("SELECT b FROM Hv HAVING b > 1 AND COUNT(*) > 0"),
            same("SELECT b FROM Hv HAVING SUM(id) > 1"),
            same("SELECT COUNT(*), b FROM Hv"),
            same("SELECT id, COUNT(*) FROM Hv GROUP BY b"),
            same("SELECT COUNT(*) FROM Hv HAVING COUNT(*) > 0"),
            same("SELECT b FROM Hv GROUP BY b HAVING COUNT(*) > 0 AND b > 1 ORDER BY b"),
            // A window is computed over the finished result, so the clause
            // deciding what the result holds cannot read one -- and the alias
            // is the half that would otherwise have been answered from the
            // projected row.
            same("SELECT b FROM Hv HAVING ROW_NUMBER() OVER (ORDER BY id) > 1"),
            same("SELECT b, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM Hv HAVING rn > 2"),
            // A window is computed over the rows the HAVING kept, not over the
            // rows the WHERE kept. The row set is the same either way and only
            // the window value says which order was used, which is why these
            // are here at all -- nothing else in this group composes the two.
            same("SELECT b, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM Hv HAVING b > 1 ORDER BY b"),
            same("SELECT b, COUNT(*) OVER () AS c FROM Hv HAVING b > 1 ORDER BY b"),
            same("SELECT b, SUM(id) OVER () AS t FROM Hv HAVING b > 1 ORDER BY b"),
            same(
                "SELECT b, ROW_NUMBER() OVER (PARTITION BY b > 2 ORDER BY id) AS rn FROM Hv \
                 HAVING b IS NOT NULL ORDER BY b",
            ),
            same("SELECT b, COUNT(*) OVER () AS c FROM Hv WHERE id < 4 HAVING b > 1 ORDER BY b"),
            same("SELECT DISTINCT b, COUNT(*) OVER () AS c FROM Hv HAVING b > 1 ORDER BY b"),
            same("SELECT b, COUNT(*) OVER () AS c FROM Hv HAVING b > 1 ORDER BY b LIMIT 1"),
            same("SELECT b, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM Hv HAVING 0"),
            same("SELECT b, COUNT(*) OVER () AS c FROM Hv HAVING c > 1"),
            // The clause order either side of the HAVING, each measured.
            same("SELECT nope FROM Hv HAVING zz > 1"),
            same("SELECT a.* FROM Hv HAVING zz > 1"),
            same("SELECT b FROM Hv HAVING nope > 1 ORDER BY alsonope"),
            same("SELECT b + 1 AS b FROM Hv HAVING b > 2 ORDER BY 1"),
            // Names before arguments: an unknown one is 1054 where a malformed
            // ESCAPE beside it would be 1210.
            same("SELECT b FROM Hv HAVING nope LIKE 'a' ESCAPE '!!'"),
            same("SELECT b FROM Hv HAVING b LIKE 'a' ESCAPE '!!'"),
            // Over an empty table, where nothing per row settles the clause.
            same("SELECT b FROM HvE HAVING b > 1"),
            same("SELECT COUNT(*) FROM HvE HAVING COUNT(*) >= 0"),
            same("SELECT b FROM HvE HAVING b LIKE 'a' ESCAPE '!!'"),
            // A FROM-less SELECT has one too, and this engine dropped it.
            same("SELECT 1 HAVING 0"),
            same("SELECT 1 AS x HAVING x > 0"),
            same("SELECT 1 HAVING nope > 0"),
            same("SELECT COUNT(*) HAVING COUNT(*) > 5"),
            same("SELECT 1 HAVING CASE WHEN 0 THEN ('a' LIKE 'b' ESCAPE '!!') ELSE 9 END > 0"),
        ],
    },
    Group {
        // Which clause an ORDER BY name resolves against, and what a position
        // counts over. #0090 and #0144 are the two halves of it: the sort ran
        // against the relation before the projection existed, so a position
        // meant the input column and a name found a column the select list
        // shadowed. Every case here answers one way if the select list wins
        // and the other way if the relation does — `id` and `b` run opposite
        // directions so no case can pass under both readings.
        name: "order by resolution",
        setup: &[
            "CREATE TABLE Ob (id INT NOT NULL, b INT NOT NULL, s VARCHAR(10) NOT NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO Ob VALUES (1,4,'x'),(2,3,'y'),(3,2,'z'),(4,1,'w')",
            "CREATE TABLE Oc (id INT NOT NULL, c INT NOT NULL, PRIMARY KEY (id))",
            "INSERT INTO Oc VALUES (1,40),(2,30),(3,20),(4,10)",
        ],
        cases: &[
            // A position names the select list. The first is #0090 exactly.
            same("SELECT JSON_ARRAY(x) FROM (SELECT 1 AS x UNION SELECT 0) t ORDER BY 1"),
            same("SELECT s, id FROM Ob ORDER BY 2"),
            same("SELECT s, id FROM Ob ORDER BY (2)"),
            same("SELECT * FROM Ob ORDER BY 3"),
            same("SELECT id FROM Ob ORDER BY 5"),
            same("SELECT id FROM Ob ORDER BY 0"),
            same("SELECT id FROM Ob ORDER BY b, 5"),
            same("SELECT * FROM Ob ORDER BY 4"),
            // The position is read before any row is, so an empty result is
            // refused rather than answered.
            same("SELECT id FROM Ob WHERE 0 ORDER BY 5"),
            // A bare name sees the select list first; the same name inside an
            // expression sees the FROM clause first. Both directions, because
            // swapping them answers half of these backwards.
            same("SELECT id AS b FROM Ob ORDER BY b"),
            same("SELECT id AS b FROM Ob ORDER BY ((b))"),
            same("SELECT id AS b FROM Ob ORDER BY b + 1"),
            same("SELECT id AS b FROM Ob ORDER BY -b"),
            same("SELECT id AS zz FROM Ob ORDER BY zz + 1"),
            same("SELECT id AS b FROM Ob ORDER BY Ob.b"),
            same("SELECT id AS id FROM Ob ORDER BY id"),
            same("SELECT b AS id FROM Ob ORDER BY id"),
            same("SELECT b AS zzz FROM Ob ORDER BY b"),
            // The half that has to keep working: a column the select list does
            // not return.
            same("SELECT id FROM Ob ORDER BY b"),
            same("SELECT id AS b FROM Ob WHERE b > 2 ORDER BY b"),
            same("SELECT id AS b FROM Ob HAVING b > 2 ORDER BY b"),
            same("SELECT id AS b FROM Ob HAVING b > 2 ORDER BY 1 DESC"),
            same("SELECT id FROM Ob ORDER BY nope"),
            // An alias shadowing a column of the *other* table in a join.
            same("SELECT Ob.id AS c FROM Ob JOIN Oc ON Oc.id = Ob.id ORDER BY c"),
            same("SELECT Ob.id AS c FROM Ob JOIN Oc ON Oc.id = Ob.id ORDER BY c + 1"),
            same("SELECT Ob.s AS id FROM Ob JOIN Oc ON Oc.id = Ob.id ORDER BY id"),
            // Two select items answering to one name: ambiguous when they are
            // different columns, and not when they are the same one or when
            // only one of them is a column at all.
            same("SELECT id AS b, b FROM Ob ORDER BY b"),
            same("SELECT id AS x, b AS x FROM Ob ORDER BY x"),
            same("SELECT id AS x, b AS x FROM Ob ORDER BY x + 0"),
            same("SELECT id AS x, b AS x FROM Ob ORDER BY 1"),
            same("SELECT b AS id, id FROM Ob ORDER BY id"),
            same("SELECT id AS b, s AS b FROM Ob ORDER BY b"),
            same("SELECT b, Ob.b FROM Ob ORDER BY b"),
            same("SELECT id, id AS id FROM Ob ORDER BY id"),
            // An item that is not a column wins outright and stops the scan,
            // from either position. DESC and a tiebreak because `LENGTH(s)` is
            // constant here: sorting on it leaves the rows as they arrived,
            // which is what sorting on `id` ascending also looks like.
            same("SELECT id AS b, LENGTH(s) AS b FROM Ob ORDER BY b DESC, id"),
            same("SELECT LENGTH(s) AS b, id AS b FROM Ob ORDER BY b DESC, id"),
            same("SELECT id AS b, s AS b, 1 AS b FROM Ob ORDER BY b"),
            same("SELECT 1 AS b, id AS b, s AS b FROM Ob ORDER BY b, id"),
            same("SELECT id AS b, 1 AS b, s AS b FROM Ob ORDER BY b DESC, id"),
            // DISTINCT has thrown the key away by the time the sort reads it.
            same("SELECT DISTINCT s FROM Ob ORDER BY id"),
            same("SELECT DISTINCT id FROM Ob ORDER BY b"),
            same("SELECT DISTINCT id FROM Ob ORDER BY id + b"),
            same("SELECT DISTINCT id + 1 AS x FROM Ob ORDER BY id"),
            same("SELECT DISTINCT id FROM Ob ORDER BY 2"),
            same("SELECT DISTINCT s FROM Ob ORDER BY id, nope"),
            same("SELECT DISTINCT s FROM Ob ORDER BY 1 DESC"),
            same("SELECT DISTINCT id AS b FROM Ob ORDER BY b"),
            same("SELECT DISTINCT id FROM Ob ORDER BY id + 1"),
            same("SELECT DISTINCT s FROM Ob ORDER BY LENGTH(s), s"),
            same("SELECT DISTINCT id, b FROM Ob ORDER BY b"),
            same("SELECT DISTINCT id + 1 AS x FROM Ob ORDER BY id + 1"),
            // A derived table is in no schema, and 8.3 names its column
            // without one rather than with a leading dot.
            same("SELECT DISTINCT s FROM (SELECT * FROM Ob) d ORDER BY id"),
            // Written a second time and spelled differently: 8.3 compares the
            // expressions, not the text they were typed as.
            same("SELECT DISTINCT id + 1 AS x FROM Ob ORDER BY id+1"),
            same("SELECT DISTINCT UPPER(s) FROM Ob ORDER BY upper(s)"),
            same("SELECT DISTINCT UPPER(s) AS u FROM Ob ORDER BY UPPER( s )"),
            // An aggregate in the clause of a query that aggregates nothing,
            // and one inside a subquery, which belongs to that subquery.
            same("SELECT s FROM Ob ORDER BY MAX(id)"),
            same("SELECT s FROM Ob ORDER BY MAX(nope)"),
            same("SELECT s FROM Ob ORDER BY MAX(id), 9"),
            same("SELECT s FROM Ob ORDER BY (SELECT MAX(id) FROM Oc), s"),
            // GROUP BY counts positions the same way, under its own clause
            // name, and after the field list.
            same("SELECT id FROM Ob GROUP BY 5"),
            same("SELECT id FROM Ob GROUP BY 0"),
            same("SELECT id FROM Ob GROUP BY 5 ORDER BY 9"),
            same("SELECT nope FROM Ob GROUP BY 5"),
            same("SELECT 5 FROM Ob GROUP BY 1"),
            same("SELECT id FROM Ob GROUP BY 1 ORDER BY 1"),
            same("SELECT id, b FROM Ob GROUP BY 1, 2 ORDER BY 2"),
            // The grouped path resolves by the same rule, and settles the
            // clause before the grouping rules.
            same("SELECT MAX(id) AS b FROM Ob GROUP BY b ORDER BY b"),
            same("SELECT MAX(id) AS b FROM Ob GROUP BY b ORDER BY b + 0"),
            same("SELECT MAX(id) AS b FROM Ob GROUP BY b ORDER BY 9"),
            same("SELECT MAX(id) AS b FROM Ob GROUP BY b ORDER BY nope"),
            same("SELECT COUNT(*) AS c, b FROM Ob GROUP BY b ORDER BY 2"),
            same("SELECT s, COUNT(*) AS n FROM Ob GROUP BY s ORDER BY 2, 1"),
            same("SELECT s, COUNT(*) AS n FROM Ob GROUP BY s ORDER BY n + 0, s"),
            same("SELECT s AS n, COUNT(*) AS s FROM Ob GROUP BY s ORDER BY s, n"),
            // A `USING` column is one column to the clause, and the
            // coalescing may only reach the columns that wildcard expanded:
            // binding the second `id` to an aliased item in front of it that
            // merely shares the name sorted on the wrong expression.
            same("SELECT LENGTH(Ob.s) AS id, Ob.*, Oc.* FROM Ob JOIN Oc USING (id) ORDER BY 2 DESC"),
            same("SELECT Ob.*, Oc.* FROM Ob JOIN Oc USING (id) ORDER BY 1"),
            // ONLY_FULL_GROUP_BY reaches the ORDER BY as well: a shadowed
            // alias falls through to a column, and a column that is neither
            // grouped nor aggregated is 1055 rather than a value read off an
            // arbitrary row of the group.
            same("SELECT s AS b, COUNT(*) AS n FROM Ob GROUP BY s ORDER BY b + 0"),
            same("SELECT s AS zz, COUNT(*) AS n FROM Ob GROUP BY s ORDER BY b + 0"),
            same("SELECT b, COUNT(*) AS n FROM Ob GROUP BY s ORDER BY b"),
            same("SELECT s AS b, COUNT(*) AS n FROM Ob GROUP BY s ORDER BY b + 0, nope"),
            same("SELECT s AS b, COUNT(*) AS n FROM Ob GROUP BY s ORDER BY b"),
            same("SELECT s, COUNT(*) AS n FROM Ob GROUP BY s ORDER BY MAX(b)"),
            same("SELECT COUNT(*) AS n FROM Ob GROUP BY s ORDER BY s"),
            // Names before arguments, in this clause as in the others.
            same("SELECT id FROM Ob ORDER BY nope LIKE 'a' ESCAPE '!!'"),
            same("SELECT id FROM Ob ORDER BY 9, s LIKE 'a' ESCAPE '!!'"),
            same("SELECT id FROM Ob GROUP BY b ORDER BY s LIKE 'a' ESCAPE '!!'"),
            same("SELECT id FROM Ob GROUP BY b ORDER BY nope LIKE 'a' ESCAPE '!!'"),
            // A set operation and a FROM-less SELECT sort a finished result,
            // so their ORDER BY names its columns and nothing else.
            same("SELECT id AS q FROM Ob UNION SELECT b FROM Ob ORDER BY q"),
            same("SELECT id AS q FROM Ob UNION SELECT b FROM Ob ORDER BY id"),
            same("SELECT id AS q FROM Ob UNION SELECT b FROM Ob ORDER BY 2"),
            same("SELECT id FROM Ob WHERE 0 UNION SELECT b FROM Ob WHERE 0 ORDER BY 5"),
            // Their ESCAPEs are settled while resolving too, which only shows
            // over a result with no rows in it: the same union with rows
            // raises 1210 through the evaluation.
            same("SELECT id FROM Ob WHERE 0 UNION SELECT b FROM Ob WHERE 0 ORDER BY s LIKE 'a' ESCAPE '!!'"),
            same("SELECT 1 AS one ORDER BY one LIKE 'a' ESCAPE '!!'"),
            same("SELECT 1 AS one ORDER BY 2"),
            same("SELECT 1 AS one ORDER BY nope"),
            same("SELECT 1 AS one ORDER BY one"),
            same("SELECT 1 AS one, 2 AS two ORDER BY 2"),
            // A correlated subquery's ORDER BY sees the enclosing row too.
            // From the WHERE clause and from the select list: the first was
            // the only spelling here while a projected scalar subquery was
            // described VAR_STRING (#0104), which would have put ten
            // divergences in the baseline for a reason that has nothing to do
            // with this clause.
            same("SELECT id FROM Ob WHERE (SELECT 1 ORDER BY id) = 1 ORDER BY id"),
            same("SELECT id, (SELECT 1 ORDER BY id) AS one FROM Ob ORDER BY id"),
            same("SELECT id FROM Ob WHERE (SELECT 1 ORDER BY nope) = 1 ORDER BY id"),
            // A window is computed over the rows in the order they reach it,
            // which is the order the sort put them in. Every window here has
            // an ORDER BY of its own or a value that does not depend on one,
            // because anything else would be comparing two query plans.
            same("SELECT ROW_NUMBER() OVER (ORDER BY b) AS rn FROM Ob ORDER BY rn DESC"),
            same("SELECT ROW_NUMBER() OVER (ORDER BY b) AS rn FROM Ob ORDER BY 1 DESC"),
            same("SELECT id, ROW_NUMBER() OVER (ORDER BY b) AS rn FROM Ob ORDER BY id DESC"),
            same("SELECT id, COUNT(*) OVER () AS c FROM Ob ORDER BY id DESC"),
            // Inside a derived table, where the LIMIT is what makes the inner
            // clause decide anything.
            same("SELECT * FROM (SELECT id AS b FROM Ob ORDER BY b LIMIT 2) d ORDER BY b"),
            // A window key beside a key the select list does not return. No
            // source row can answer a window, so the sort is finished over the
            // projected result — and finishing it by re-deriving *every* key
            // from that result refused these for `s`, which the output has no
            // column for. Each key alone answers, so only the mixture failed.
            //
            // Every window here still carries its own ORDER BY, for the reason
            // the block above gives: `OVER ()` numbers the rows in whatever
            // order they reach it, which is a query plan and not an answer.
            same("SELECT id, ROW_NUMBER() OVER (ORDER BY b) AS rn FROM Ob ORDER BY 2, s"),
            same("SELECT ROW_NUMBER() OVER (ORDER BY b) AS rn, id FROM Ob ORDER BY s, 1"),
            same("SELECT id, ROW_NUMBER() OVER (ORDER BY b) AS rn FROM Ob ORDER BY rn DESC, s"),
            same("SELECT id, ROW_NUMBER() OVER (ORDER BY b) AS rn FROM Ob ORDER BY 2, s LIMIT 2"),
            // DISTINCT collapses rows after that sort rather than before it.
            same("SELECT DISTINCT s, ROW_NUMBER() OVER (ORDER BY b) AS rn FROM Ob ORDER BY rn DESC"),
            // The same retry with no window in sight: an alias inside a larger
            // expression is substituted for what the select list wrote, which
            // a source row cannot answer either.
            same("SELECT id, id * 10 AS q FROM Ob ORDER BY CASE WHEN q > 20 THEN 0 ELSE 1 END, s"),
            // Two qualified wildcards over a USING join put the joined column
            // in the output twice, so a bare name in the ORDER BY is
            // ambiguous — in either order of the wildcards. Only the bare `*`
            // coalesces. This has been argued both ways from memory, which is
            // why it is here.
            same("SELECT Ob.*, Oc.* FROM Ob JOIN Oc USING (id) ORDER BY id"),
            same("SELECT Oc.*, Ob.* FROM Ob JOIN Oc USING (id) ORDER BY id"),
            // One wildcard leaves one `id` in the output, so the same clause
            // answers; a position names one output column outright and is
            // never ambiguous, which the case above `ORDER BY 2 DESC` already
            // asks.
            same("SELECT Ob.* FROM Ob JOIN Oc USING (id) ORDER BY id"),
            same("SELECT Ob.*, Oc.c FROM Ob JOIN Oc USING (id) ORDER BY id"),
            // The WHERE reads the FROM clause, where USING really has made the
            // two columns one, so the same name answers there.
            same("SELECT Ob.*, Oc.* FROM Ob JOIN Oc USING (id) WHERE id = 1"),
        ],
    },
    Group {
        // The database part of a three-part name was compared against nothing.
        // `SELECT nodb.Qt.b FROM Qt` answered rows and `SELECT nodb.Qt.*`
        // answered every column, where 8.3 raises 1054 and 1051 — so a
        // statement written against another schema silently read this one.
        //
        // The database is spelled `differential` throughout, which is the
        // database `main.rs` creates for every group. A case naming a second
        // one would have to create it, and a database outlives the group that
        // made it: the reset drops only this one.
        name: "a database qualifier",
        setup: &[
            "CREATE TABLE Qt (a INT NULL, b INT NULL)",
            "INSERT INTO Qt VALUES (1,2),(3,4)",
            "CREATE TABLE Qk (a INT NULL, c INT NULL)",
            "INSERT INTO Qk VALUES (1,9)",
            // Empty on purpose: a refusal that only happens while a row is
            // being evaluated does not happen at all over an empty relation,
            // which is the shape three other issues under #0009 have been.
            "CREATE TABLE Qe (a INT NULL, b INT NULL)",
            "CREATE VIEW Qv AS SELECT b FROM Qt",
        ],
        cases: &[
            // The two the issue was written from, and their legal twins.
            same("SELECT nodb.Qt.b FROM Qt"),
            same("SELECT nodb.Qt.* FROM Qt"),
            same("SELECT differential.Qt.b FROM Qt ORDER BY 1"),
            same("SELECT differential.Qt.* FROM Qt ORDER BY 1"),
            same("SELECT differential.Qt.b FROM differential.Qt ORDER BY 1"),
            same("SELECT Qt.b FROM differential.Qt ORDER BY 1"),
            // An alias renames the table part and leaves the database alone.
            same("SELECT differential.x.b FROM Qt x ORDER BY 1"),
            same("SELECT nodb.x.b FROM Qt x"),
            same("SELECT differential.x.* FROM Qt x ORDER BY 1"),
            same("SELECT nodb.x.* FROM Qt x"),
            // Nothing evaluates the reference in any of these, which is where
            // a check living in the row path stops running.
            same("SELECT nodb.Qe.b FROM Qe"),
            same("SELECT differential.Qe.b FROM Qe"),
            same("SELECT nodb.Qt.b FROM Qt WHERE 1 = 0"),
            same("SELECT nodb.Qt.b FROM Qt LIMIT 0"),
            same("SELECT CASE WHEN 0 THEN nodb.Qt.b ELSE 9 END FROM Qt"),
            same("SELECT nodb.Qe.* FROM Qe"),
            // Every clause that resolves a name, right and wrong.
            same("SELECT b FROM Qt WHERE nodb.Qt.a = 1"),
            same("SELECT b FROM Qt WHERE differential.Qt.a = 1"),
            same("SELECT b FROM Qt GROUP BY nodb.Qt.b"),
            same("SELECT b FROM Qt GROUP BY differential.Qt.b ORDER BY 1"),
            same("SELECT b FROM Qt GROUP BY b HAVING nodb.Qt.b > 0"),
            same("SELECT b FROM Qt GROUP BY b HAVING differential.Qt.b > 0 ORDER BY 1"),
            same("SELECT b FROM Qt ORDER BY nodb.Qt.b"),
            same("SELECT b FROM Qt ORDER BY differential.Qt.b"),
            same("SELECT Qt.b FROM Qt JOIN Qk ON nodb.Qt.a = Qk.a"),
            same("SELECT Qt.b FROM Qt JOIN Qk ON differential.Qt.a = Qk.a"),
            same("SELECT b FROM Qt WHERE a IN (SELECT a FROM Qk WHERE nodb.Qt.b = 2)"),
            same("SELECT b FROM Qt WHERE a IN (SELECT a FROM Qk WHERE differential.Qt.b = 2)"),
            same("SELECT nodb.Qt.b FROM Qt UNION SELECT 1"),
            // A view is in a database; the query behind it is not the view.
            same("SELECT differential.Qv.b FROM Qv ORDER BY 1"),
            same("SELECT nodb.Qv.b FROM Qv"),
            same("SELECT differential.Qv.* FROM Qv ORDER BY 1"),
            same("SELECT nodb.Qv.* FROM Qv"),
            // A derived table, a CTE and a JSON_TABLE are in no database, and
            // the two clauses read that absence in *opposite* directions: the
            // column form takes any database and the wildcard takes none, the
            // right one included.
            same("SELECT nodb.d.b FROM (SELECT b FROM Qt) d ORDER BY 1"),
            same("SELECT differential.d.b FROM (SELECT b FROM Qt) d ORDER BY 1"),
            same("SELECT differential.d.* FROM (SELECT b FROM Qt) d"),
            same("WITH q AS (SELECT b FROM Qt) SELECT nodb.q.b FROM q ORDER BY 1"),
            same("WITH q AS (SELECT b FROM Qt) SELECT differential.q.* FROM q"),
            same(
                "SELECT nodb.jt.v FROM JSON_TABLE('[1,2]', '$[*]' COLUMNS(v INT PATH '$')) jt \
                 ORDER BY 1",
            ),
            same(
                "SELECT differential.jt.* FROM \
                 JSON_TABLE('[1,2]', '$[*]' COLUMNS(v INT PATH '$')) jt",
            ),
            // information_schema is a database and answers to its own name.
            same(
                "SELECT information_schema.tables.TABLE_NAME FROM information_schema.tables \
                 WHERE TABLE_SCHEMA = 'differential' AND TABLE_NAME = 'Qk'",
            ),
            same("SELECT differential.tables.TABLE_NAME FROM information_schema.tables LIMIT 1"),
            // The writes, last, because they change what the reads above see.
            // A target resolved by its last part alone wrote to this database
            // whatever database was written in front of it, and reported rows
            // affected exactly as a correct statement would.
            same("INSERT INTO Qt (nodb.Qt.a) VALUES (9)"),
            same("INSERT INTO Qt (Qk.a) VALUES (9)"),
            same("INSERT INTO Qt (differential.Qt.a) VALUES (9)"),
            // No unique key on `Qt`, so nothing collides and the assignment is
            // never evaluated — 8.3 still resolves the target and refuses.
            same("INSERT INTO Qt (a,b) VALUES (1,2) ON DUPLICATE KEY UPDATE nodb.Qt.b = 1"),
            same(
                "INSERT INTO Qt (a,b) VALUES (1,2) ON DUPLICATE KEY UPDATE \
                 differential.Qt.b = 1",
            ),
            same("UPDATE Qt x JOIN Qk ON x.a = Qk.a SET nodb.x.b = 1"),
            // Every SET below changes every row it touches, which takes some
            // arranging: each case runs twice — once per protocol — and
            // MySQL's affected count is the number of rows it *changed*, so
            // `SET b = 5` reports 0 on the second run and `SET b = b + 1`
            // reports nothing for a row where `b` is NULL. Either would make
            // the case compare this engine's counting rule against MySQL's
            // rather than the qualifier it is here for, and those two rules
            // differ — the engine counts the rows it matched. That is its own
            // divergence and not this one.
            same(
                "UPDATE Qt x JOIN Qk ON x.a = Qk.a SET \
                 differential.x.b = COALESCE(x.b, 0) + 1",
            ),
            // The single-table SET target, which read its last part alone —
            // so the table half of the qualifier was dropped with the
            // database. Over the empty table too, where the targets are
            // resolved before any row is considered.
            same("UPDATE Qt SET nodb.Qt.b = 1"),
            same("UPDATE Qt SET Qk.b = 1"),
            same("UPDATE Qe SET nodb.Qe.b = 1"),
            same("UPDATE Qt SET differential.Qt.b = COALESCE(differential.Qt.b, 0) + 1"),
            same("DELETE FROM Qt WHERE differential.Qt.a = 9"),
            same("SELECT a, b FROM Qt ORDER BY a, b"),
        ],
    },
    Group {
        // The write the group above missed. A multi-table DELETE read its
        // target by the last part of the name, so `DELETE nodb.Md FROM Md JOIN
        // Mk ON Md.a = Mk.a` removed rows from the current schema and reported
        // them affected exactly as a correct statement would — a DELETE aimed
        // at another schema emptying this one.
        //
        // 8.3's code here is 1109 `Unknown table 'Md' in MULTI DELETE`, not
        // the 1054 the column reference gets, and it is the same 1109 for a
        // target the FROM clause simply does not name — where this engine said
        // 1146.
        //
        // One database again, for the reason the group above gives: the reset
        // drops `differential` and nothing else, so a case naming a second one
        // would leave it behind for every group after it.
        name: "a multi-table DELETE target's database",
        setup: &[
            "CREATE TABLE Md (a INT NULL, b INT NULL)",
            "INSERT INTO Md VALUES (1,2),(3,4)",
            "CREATE TABLE Mk (a INT NULL, c INT NULL)",
            "INSERT INTO Mk VALUES (1,9)",
            // Empty on purpose, as `Qe` is above: a refusal raised while a row
            // is evaluated is not raised at all when there are none.
            "CREATE TABLE Me (a INT NULL, b INT NULL)",
            // Its own table for the deletes that succeed, so the refusals
            // above them can be read without tracking what is left in `Md`.
            "CREATE TABLE Mg (a INT NULL)",
            "INSERT INTO Mg VALUES (1),(2),(3),(4),(5),(6),(7)",
            "CREATE TABLE Mh (a INT NULL)",
            "INSERT INTO Mh VALUES (1),(2),(3),(4),(5),(6),(7)",
        ],
        cases: &[
            // The statement the issue was written from. A database that
            // *exists* and is not the one the FROM clause names belongs in the
            // engine's own tests rather than here, because the only other
            // database this harness can name is `information_schema` — and
            // 8.3 answers 1044 for a DELETE aimed at that one, checking the
            // privilege before it resolves the target.
            same("DELETE nodb.Md FROM Md JOIN Mk ON Md.a = Mk.a"),
            // An alias takes the item out of its database here, which is the
            // opposite of what it does to a column reference: `SELECT
            // differential.x.b FROM Qt x` answers in the group above and the
            // right database in front of an alias is 1109 in this one.
            same("DELETE differential.x FROM Md x JOIN Mk ON x.a = Mk.a"),
            same("DELETE nodb.x FROM Md x JOIN Mk ON x.a = Mk.a"),
            // ... including where the alias is spelled the same as the table
            // it renames, so this is not the alias merely replacing the table
            // part of the name.
            same("DELETE differential.Md FROM Md Md JOIN Mk ON Md.a = Mk.a"),
            // Which of several targets is wrong, and which one is reported.
            same("DELETE nodb.Md, Mk FROM Md JOIN Mk ON Md.a = Mk.a"),
            same("DELETE Md, nodb.Mk FROM Md JOIN Mk ON Md.a = Mk.a"),
            same("DELETE nodb.Md, nodb2.Mk FROM Md JOIN Mk ON Md.a = Mk.a"),
            // A repeated target is settled over the whole list before any of
            // it is resolved: the first two are 1066 rather than the 1109 that
            // resolving the first target would give, and the third is 1066 on
            // its last target rather than 1109 on its middle one. Two
            // spellings of one database are one target, and two different
            // databases are two — so the last of these is 1109 again.
            same("DELETE nodb.Md, nodb.Md FROM Md JOIN Mk ON Md.a = Mk.a"),
            same("DELETE differential.Md, Md FROM Md JOIN Mk ON Md.a = Mk.a"),
            same("DELETE Md, Nope, differential.Md FROM Md JOIN Mk ON Md.a = Mk.a"),
            same("DELETE nodb.Md, nodb2.Md FROM Md JOIN Mk ON Md.a = Mk.a"),
            // A target with no qualifier at all that the FROM clause does not
            // name is the same 1109, which this engine answered 1146.
            same("DELETE Nope FROM Md JOIN Mk ON Md.a = Mk.a"),
            // A dot inside a name is not a qualifier, so this names one
            // database called `my.db` rather than a table in `my`.
            same("DELETE `my.db`.Md FROM Md JOIN Mk ON Md.a = Mk.a"),
            // More parts than the grammar has levels for: 1064 on both, rather
            // than the last two parts being read and the rows deleted.
            same("DELETE a.b.c FROM Md JOIN Mk ON Md.a = Mk.a"),
            // The `USING` spelling puts its targets in the other half of the
            // statement, and read them through the other dropped qualifier.
            same("DELETE FROM nodb.Md USING Md JOIN Mk ON Md.a = Mk.a"),
            same("DELETE FROM nodb.Md, Mk USING Md JOIN Mk ON Md.a = Mk.a"),
            // A target of that spelling may not be renamed: the names come
            // before USING and the relation with its aliases comes after, so
            // all of these are 1064 on 8.3 — the two with the database spelled
            // *right* included. This engine's parser takes a whole table
            // reference there, and reading the alias as the target is what
            // took the database off it: the third of these deleted from the
            // current schema and reported the row affected.
            same("DELETE FROM Md m USING Md m JOIN Mk ON m.a = Mk.a"),
            same("DELETE FROM differential.Md m USING Md m JOIN Mk ON m.a = Mk.a"),
            same("DELETE FROM nodb.Md m USING Md m JOIN Mk ON m.a = Mk.a"),
            same("DELETE FROM Md AS m USING Md m JOIN Mk ON m.a = Mk.a"),
            same("DELETE FROM a.b.c x USING Md x JOIN Mk ON x.a = Mk.a"),
            same("DELETE FROM Md, Mk k USING Md JOIN Mk ON Md.a = Mk.a"),
            // `a.*` is the canonical spelling of a target; the star carries no
            // meaning the name does not, and what is left still has a database
            // in front of it.
            same("DELETE nodb.Md.* FROM Md JOIN Mk ON Md.a = Mk.a"),
            // A target list over a single table is a multi-table DELETE too.
            same("DELETE nodb.Md FROM Md"),
            // Nothing evaluates a row in any of these, which is where a check
            // living in the row path stops running.
            same("DELETE nodb.Me FROM Me JOIN Mk ON Me.a = Mk.a"),
            same("DELETE nodb.Md FROM Md JOIN Mk ON Md.a = Mk.a WHERE 1 = 0"),
            same("DELETE nodb.Md FROM Md JOIN Mk ON Md.a = Mk.a WHERE CASE WHEN 0 THEN 1 ELSE 0 END"),
            // The targets before the predicate, which is 8.3's order: this is
            // 1109 there and not the WHERE's own 1054.
            same("DELETE nodb.Md FROM Md JOIN Mk ON Md.a = Mk.a WHERE nope = 1"),
            // Nothing above has written, so `Md` is still as the setup left it
            // — the half of this that a refusal cannot show.
            same("SELECT a, b FROM Md ORDER BY a, b"),
            // The spellings that must keep deleting, each taking one row of
            // its own so the count is 1 rather than whatever is left. Each
            // case runs twice, so the second pass finds its row already gone
            // and affects none — on both servers.
            same("DELETE differential.Mg FROM Mg JOIN Mh ON Mg.a = Mh.a WHERE Mg.a = 1"),
            same("DELETE Mg FROM differential.Mg JOIN Mh ON differential.Mg.a = Mh.a WHERE Mg.a = 2"),
            same("DELETE differential.Mg FROM differential.Mg JOIN Mh ON Mg.a = Mh.a WHERE Mg.a = 3"),
            same("DELETE x FROM Mg x JOIN Mh ON x.a = Mh.a WHERE x.a = 4"),
            same("DELETE Mg FROM Mg Mg JOIN Mh ON Mg.a = Mh.a WHERE Mg.a = 5"),
            same("DELETE FROM differential.Mg USING Mg JOIN Mh ON Mg.a = Mh.a WHERE Mg.a = 6"),
            // The star spelling, over the row the six above left alone — a
            // seventh, because a join whose other side has been emptied
            // deletes nothing however the target is written, which would make
            // this case agree without asking anything.
            same("DELETE differential.Mh.* FROM Mg JOIN Mh ON Mg.a = Mh.a WHERE Mh.a = 7"),
            same("SELECT a FROM Mg ORDER BY a"),
            same("SELECT a FROM Mh ORDER BY a"),
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
        // `FLOAT(M,D)` and `DOUBLE(M,D)` are not the one-argument forms with a
        // display rule attached. MySQL rounds the stored value to D places on
        // the way in, checks it against the M digits *after* that rounding,
        // and then shows exactly D places wherever the column itself is read.
        // #0075.
        //
        // Wherever the *column* is read, and no further: MySQL carries that D
        // forward as an expression's `decimals`, so `f + 0` and `SUM(f)` still
        // show four places there and `AVG(f)` shows eight, and this engine has
        // no per-expression `decimals` to carry. There is deliberately no case
        // for those below — they would be divergences, and the baseline is
        // empty. `REAL(M,D)` has none either: MySQL reads it as
        // `double(10,4)` and `sqlparser` will not parse the spelling at all,
        // so the engine answers 1064.
        name: "a float declared with a scale",
        setup: &[
            "CREATE TABLE Fx (id INT NOT NULL, f FLOAT(10,4) NULL, d DOUBLE(10,4) NULL, \
             n FLOAT(6,2) NULL, pf FLOAT NULL, PRIMARY KEY (id))",
            // 2.00005 tells the rounding order apart: its `f64` falls just
            // below the half and its `f32` just above, so a value narrowed
            // before it was rounded comes out 2.0001 instead of 2.0000.
            // 0.125 is an exact half and pins the tie rule, which is `rint`'s
            // and rounds to the even digit.
            "INSERT INTO Fx VALUES (1, 1.234567890123, 1.234567890123, 2.00005, 1.234567890123), \
             (2, 0.1, 0.1, 0.125, 0.1), (3, 1.5, 1.5, 9999.994, 16777217), \
             (4, -3.14159265, -3.14159265, NULL, NULL)",
            "CREATE VIEW Fv AS SELECT f, d FROM Fx",
            // A scale wide enough to run past what a double distinguishes,
            // where padding the binary expansion and padding the shortest
            // round-trip stop being the same string. The `Ba` case below only
            // declared these widths and never stored a value in them.
            "CREATE TABLE Fw (id INT NOT NULL, p DOUBLE(20,17) NULL, q DOUBLE(255,30) NULL, \
             r FLOAT(40,2) NULL, z FLOAT(10,2) NULL, PRIMARY KEY (id))",
            "INSERT INTO Fw VALUES (1, 0.1, 1.5, 1e30, -0.001), (2, 0.1, 0.5, 1e30, -0.006)",
            // Two scales on the same code, and two codes: the merged column
            // takes the wider scale either way round and rounds both branches
            // into it.
            "CREATE TABLE Fu (f FLOAT(10,4) NULL, d DOUBLE(10,4) NULL, a FLOAT(12,2) NULL, \
             h DOUBLE(12,6) NULL, pd DOUBLE NULL)",
            "INSERT INTO Fu VALUES (0.1, 0.1, 0.1, 0.1, 0.1)",
        ],
        cases: &[
            same("SELECT f, d, n FROM Fx ORDER BY id"),
            // The places are a rendering; the rounding is of the value. Only
            // somewhere that reads the number rather than the column can tell
            // the two apart, and a document is that place.
            same("SELECT JSON_OBJECT('f', f, 'd', d) FROM Fx ORDER BY id"),
            same("SELECT CAST(f AS DECIMAL(20,10)), CAST(d AS DECIMAL(20,10)) FROM Fx ORDER BY id"),
            // The same conversion over a *plain* FLOAT, which was reading the
            // six digits the column displays rather than the double behind
            // them and answered 1.2345700000 for 1.2345678806304932.
            same("SELECT CAST(pf AS DECIMAL(20,10)) FROM Fx ORDER BY id"),
            // A derived table and a set operation both carry the declared
            // scale forward, so the places survive being materialised.
            same("SELECT f FROM (SELECT f FROM Fx) s ORDER BY f"),
            same("SELECT f FROM Fx UNION ALL SELECT f FROM Fx ORDER BY 1"),
            // A view is the one of those that runs a whole query of its own,
            // and its result used to be shaped for its own column codes before
            // the outer query read it: the `double(10,4)` came back without
            // its places and the `float(10,4)` beside it kept them.
            same("SELECT f, d FROM Fv ORDER BY f"),
            same("SELECT COUNT(*), f FROM Fx GROUP BY f ORDER BY f"),
            // M digits with D after the point stops a `FLOAT(6,2)` below
            // 10000, and the check is against the rounded value: 9999.994 is
            // in the row above and 9999.995 is 1264.
            same("INSERT INTO Fx (id, n) VALUES (9, 9999.995)"),
            same("INSERT INTO Fx (id, n) VALUES (9, 12345.678)"),
            same("INSERT INTO Fx (id, d) VALUES (9, -123456789.5)"),
            same("UPDATE Fx SET n = 10000.00 WHERE id = 1"),
            same("SELECT n FROM Fx WHERE n > 1 ORDER BY n"),
            // M and D are limits, not decoration: a scale is a count of
            // characters written per value, so an unchecked one is a
            // million-character number from one statement. The codes prove the
            // order — the scale is checked before the precision, so
            // `FLOAT(300,31)` is 1425 and not 1439.
            same("CREATE TABLE B1 (x FLOAT(10,31))"),
            same("CREATE TABLE B2 (x DOUBLE(10,31))"),
            same("CREATE TABLE B3 (x DECIMAL(10,1000000))"),
            same("CREATE TABLE B4 (x FLOAT(300,31))"),
            same("CREATE TABLE B5 (x DECIMAL(66,31))"),
            same("CREATE TABLE B6 (x FLOAT(300,2))"),
            same("CREATE TABLE B7 (x DOUBLE(256,2))"),
            same("CREATE TABLE B8 (x DECIMAL(66,2))"),
            same("CREATE TABLE B9 (x FLOAT(2,5))"),
            same("SELECT p, q, r FROM Fw ORDER BY id"),
            // A negative value that rounds to zero loses its sign, and not
            // only in the printing: 8.3 stores the positive zero, so the
            // document and the wire lose it too. The second row keeps its sign
            // because its rounding does not reach zero.
            same("SELECT z, JSON_ARRAY(z) FROM Fw ORDER BY id"),
            // A set operation is materialised through a field of the merged
            // type, so both branches come back at the wider scale — and the
            // rounding happens before the de-duplication, which is what makes
            // the UNION below one row rather than two.
            same("SELECT f FROM Fu UNION ALL SELECT d FROM Fu ORDER BY 1"),
            same("SELECT f FROM Fu UNION ALL SELECT h FROM Fu ORDER BY 1"),
            same("SELECT f FROM Fu UNION ALL SELECT a FROM Fu ORDER BY 1"),
            same("SELECT a FROM Fu UNION ALL SELECT f FROM Fu ORDER BY 1"),
            same("SELECT f FROM Fu UNION SELECT d FROM Fu ORDER BY 1"),
            // A plain DOUBLE has no scale to merge, so neither has the column.
            same("SELECT f FROM Fu UNION ALL SELECT pd FROM Fu ORDER BY 1"),
            // The rounding is of the value and not of the printing, which only
            // shows once the merged result feeds another query: a document
            // takes the number and ignores the places.
            same(
                "SELECT JSON_ARRAY(x) FROM \
                 (SELECT f AS x FROM Fu UNION ALL SELECT d FROM Fu) t ORDER BY 1",
            ),
            same(
                "SELECT COUNT(DISTINCT x) FROM \
                 (SELECT f AS x FROM Fu UNION ALL SELECT d FROM Fu) t",
            ),
            // A cast declares a precision and a scale too, and unchecked they
            // were a million-digit answer from one SELECT. Its rule is not the
            // column's and neither is its order: the precision first, then
            // M >= D, and only then the scale.
            same("SELECT CAST(1.5 AS DECIMAL(10,31))"),
            same("SELECT CAST(1.5 AS DECIMAL(2,5))"),
            same("SELECT CAST(1.5 AS DECIMAL(10,1000000))"),
            same("SELECT CAST(1.5 AS DECIMAL(66,2))"),
            same("SELECT CAST(1.5 AS DECIMAL(70,40))"),
            same("SELECT CAST(1.5 AS DECIMAL(65,31))"),
            same("SELECT CAST(1.5 AS DECIMAL(65,30))"),
            // Every path that declares a column, not just CREATE TABLE.
            same("ALTER TABLE Fx ADD COLUMN z DECIMAL(10,31)"),
            same("ALTER TABLE Fx MODIFY COLUMN n FLOAT(300,2)"),
            // And the widest each type really allows still goes through.
            same("CREATE TABLE Ba (x FLOAT(255,30), y DOUBLE(255,30), z DECIMAL(65,30))"),
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
            // Widths not asked, and the reason is MySQL's rather than this
            // engine's: a `DATETIME(3)` read through a view is 92 there — its
            // 23 characters times the four bytes utf8mb4 needs for one — where
            // the same column read from the table is 23 with the binary
            // character set. The engine sends 23 either way, which is the
            // number MySQL sends for the column it is a view of.
            // The same view column declines both questions, and for the same
            // quirk: a `DATETIME(3)` that has been through a temporary table
            // is `cs=45` on 8.3 -- characters -- where the column it is a view
            // of is binary, and its width is that charset's four bytes a
            // character times its 23.
            same("SELECT * FROM V")
                .without_widths()
                .without_value_kind(),
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
            // `SELECT NULL UNION SELECT NULL` and its family live in the
            // "a materialised null column" group below, which is where the
            // measurement that made them askable is written down. This note
            // used to say they were unaskable because MySQL answers the
            // statement two ways; it answers the *result set* one way in both
            // protocols, and the harness reads no other kind of metadata.
            // #0073.
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
        // A column that never holds anything but NULL has no type, and no
        // temporary-table field can carry one — so materialising it gives it
        // `binary(0)`. Which statements materialise is the whole question, and
        // it is not the one the extra machinery suggests: sorting, grouping,
        // de-duplicating and limiting all leave `MYSQL_TYPE_NULL` alone.
        //
        // What this group cannot ask, and #0073 records: MySQL's
        // `COM_STMT_PREPARE` answers `MYSQL_TYPE_NULL` for the *outermost* set
        // operation of a statement, because that temporary table is not built
        // until it runs. The harness reads result-set metadata, which MySQL
        // answers `binary(0)` in both protocols, so a case here compares the
        // answer the two servers agree the question has — asking the other one
        // would need the harness to read prepare metadata, which it does not.
        name: "a materialised null column",
        setup: &[
            "CREATE VIEW VNull AS SELECT NULL AS c",
            "CREATE VIEW VNullUnion AS SELECT NULL AS c UNION SELECT NULL",
        ],
        cases: &[
            // Unmaterialised, and the reference point for everything below.
            same("SELECT NULL"),
            // Materialised by a set operation, whichever one and however many
            // branches.
            same("SELECT NULL UNION SELECT NULL"),
            same("SELECT NULL UNION ALL SELECT NULL ORDER BY 1"),
            same("SELECT NULL UNION SELECT NULL UNION SELECT NULL"),
            same("SELECT NULL INTERSECT SELECT NULL"),
            same("SELECT NULL EXCEPT SELECT NULL"),
            same("(SELECT NULL) UNION (SELECT NULL)"),
            same("SELECT NULL UNION SELECT NULL ORDER BY 1"),
            same("SELECT NULL UNION SELECT NULL LIMIT 1"),
            // Brackets round the whole statement change the parse and not the
            // answer, at any depth and with the ORDER BY on either side.
            same("(SELECT NULL UNION SELECT NULL)"),
            same("((SELECT NULL UNION SELECT NULL))"),
            same("(SELECT NULL UNION SELECT NULL) ORDER BY 1"),
            same("(SELECT NULL UNION SELECT NULL LIMIT 1)"),
            same("(SELECT NULL)"),
            same("((SELECT NULL))"),
            same("(SELECT NULL) ORDER BY 1"),
            // One typed branch settles it, and a chain is materialised once
            // over all its terms — so the NULL pair does not become a
            // `binary(0)` that then widens against the integer. Bracketing
            // the pair does not change it either, which is what says the rule
            // is not associativity.
            same("SELECT NULL UNION SELECT NULL UNION SELECT 1 ORDER BY 1"),
            same("(SELECT NULL UNION SELECT NULL) UNION SELECT 1 ORDER BY 1"),
            same("SELECT 1 UNION (SELECT NULL UNION SELECT NULL) ORDER BY 1"),
            same("SELECT NULL UNION SELECT 1 UNION SELECT NULL ORDER BY 1"),
            // Not materialised: the clause is not what does it.
            same("SELECT NULL ORDER BY 1"),
            same("SELECT NULL GROUP BY 1"),
            same("SELECT DISTINCT NULL"),
            same("SELECT NULL LIMIT 1"),
            // The other way a result is materialised, with and without a set
            // operation inside it.
            same("SELECT * FROM (SELECT NULL) d"),
            same("SELECT * FROM (SELECT NULL UNION SELECT NULL) d"),
            same("WITH c AS (SELECT NULL) SELECT * FROM c"),
            same("WITH c AS (SELECT NULL UNION SELECT NULL) SELECT * FROM c"),
            same("SELECT * FROM VNull"),
            same("SELECT * FROM VNullUnion"),
            // A table value constructor answers both halves at once: typeless
            // on its own, `binary(0)` once something materialises it.
            same("VALUES ROW(NULL)"),
            same("VALUES ROW(NULL), ROW(NULL)"),
            same("SELECT * FROM (VALUES ROW(NULL)) v"),
            same("SELECT * FROM (VALUES ROW(NULL), ROW(NULL)) v"),
            same("VALUES ROW(NULL) UNION VALUES ROW(NULL)"),
            same("VALUES ROW(NULL), ROW(1)"),
            same("SELECT * FROM (VALUES ROW(NULL), ROW(1)) v"),
            // A materialised branch beside an unmaterialised one keeps the
            // materialised type, since the NULL branch still contributes none.
            same("SELECT * FROM (SELECT NULL UNION SELECT NULL) d UNION SELECT NULL"),
            // `binary(0)` and not merely "a string": the type code is shared
            // with CHAR and the charset is the difference, so the spelling is
            // asked for where a client can read it.
            //
            // `DATABASE()` rather than the name spelled out, because a group's
            // database is named by the harness: a literal that stopped
            // matching would select no rows on *both* servers, compare equal,
            // and report a case that asked nothing as a case that agreed.
            same(
                "SELECT TABLE_NAME, DATA_TYPE, COLUMN_TYPE, CHARACTER_MAXIMUM_LENGTH, IS_NULLABLE \
                 FROM information_schema.columns WHERE TABLE_SCHEMA = DATABASE() \
                 AND TABLE_NAME IN ('VNull', 'VNullUnion') ORDER BY TABLE_NAME",
            ),
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
                        // Bytes against characters not asked, here and in the four cases
            // below it: MySQL takes a string function's collation from its
            // operand, and a bare NULL is binary there, so `UPPER(NULL)` comes
            // back as bytes. This engine has no collation to propagate and
            // answers characters. What the group is about -- that NULL travels
            // through the function -- is compared either way.
            same("SELECT UPPER(NULL), LOWER(NULL), UCASE(NULL), LCASE(NULL)").without_value_kind(),
            same("SELECT TRIM(NULL), LTRIM(NULL), RTRIM(NULL), REVERSE(NULL), SPACE(NULL)").without_value_kind(),
            same("SELECT LENGTH(NULL), CHAR_LENGTH(NULL), OCTET_LENGTH(NULL)"),
            same("SELECT LEFT(NULL, 2), RIGHT(NULL, 2), SUBSTRING(NULL, 1, 2), MID(NULL, 1, 2)").without_value_kind(),
            same("SELECT LPAD(NULL, 3, 'x'), RPAD(NULL, 3, 'x'), REPLACE(NULL, 'a', 'b')").without_value_kind(),
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
            same("SELECT SOUNDEX(NULL)").without_value_kind(),
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
            // A binary column of its own rather than a column added to `S`:
            // SOUNDEX reads a binary string by a different rule from a text
            // one, and every case above selects from `S` by name.
            "CREATE TABLE SB (id INT NOT NULL, b VARBINARY(32) NULL, PRIMARY KEY (id))",
            "INSERT INTO SB VALUES (1,'Robert'),(2,'éabc')",
        ],
        cases: &[
            same("SELECT CONCAT(t, '!'), UPPER(t), LOWER(t), LENGTH(t) FROM S ORDER BY id"),
            same("SELECT TRIM(t), SUBSTRING(t, 2, 3), REPLACE(t, 'l', 'L') FROM S ORDER BY id"),
            // Where MySQL's Soundex parts company with the description of it:
            // a vowel does not break a run, the first letter seeds that run,
            // and the answer is not truncated to four.
            same("SELECT SOUNDEX('Tymczak'), SOUNDEX('Pfister'), SOUNDEX('Ashcraft')"),
            same("SELECT SOUNDEX('Robert'), SOUNDEX('Bhbb'), SOUNDEX(''), SOUNDEX(123)"),
            // The letter test past ASCII, which is a range over the codepoint
            // and not a Unicode property: `µ` and `ª` are alphabetic to Rust
            // and below MySQL's U+00C0 threshold, and `×` is the reverse. The
            // character that passes is emitted whole, unfolded, and codes
            // zero. #0074 recorded this as unreachable on a reading taken
            // through a latin1 connection, where each UTF-8 byte arrives as
            // its own character and a lead byte passes the same test.
            same("SELECT SOUNDEX('éabc'), SOUNDEX('€abc'), SOUNDEX('µabc'), SOUNDEX('×abc')"),
            // Only the leading character can differ: everywhere else a
            // non-ASCII letter codes zero, like a vowel, and writes nothing.
            same("SELECT SOUNDEX('abc€'), SOUNDEX('aé'), SOUNDEX('b€b'), SOUNDEX('café')"),
            // A binary string is the other letter test. The binary charset's
            // ctype table classifies only ASCII, so a high byte is skipped as
            // punctuation where the same character as text is a letter coding
            // zero — which is why the second of these is empty here and
            // `é000` as text.
                        // Bytes against characters not asked: the operand is explicitly
            // binary -- `BINARY 'x'`, `CAST(x AS BINARY)`, a `BLOB` column --
            // and MySQL carries that collation into the answer where this
            // engine has none to carry.
            same("SELECT SOUNDEX(CAST('éabc' AS BINARY)), SOUNDEX(CAST('éé' AS BINARY))").without_value_kind(),
            same("SELECT SOUNDEX(BINARY 'éabc'), SOUNDEX(CAST('Ashcraft' AS BINARY))").without_value_kind(),
            same("SELECT id, SOUNDEX(b) FROM SB ORDER BY id").without_value_kind(),
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
        // #0093: the escape character was read out of the *rendering* of the
        // ESCAPE literal, so it was the opening quote and every clause here
        // escaped with `'`. A wrong answer and a silent one -- LIKE hands back
        // a plausible 0 or 1 either way -- which is why the corpus now asks
        // the clause every way 8.3 answers it rather than only the one that
        // was reported.
        //
        // Every case in this group is ASCII and same-case on both sides of the
        // LIKE. That is deliberate and not incidental: LIKE is case-sensitive
        // in this engine and case-*insensitive* on 8.3 under the default
        // collation, so `SELECT 'ABC' LIKE 'a%'` is 1 there and 0 here. That
        // is a gap of its own -- the engine has no collation to propagate, and
        // folding LIKE unconditionally would break `BINARY 'A' LIKE 'a'`,
        // which it answers correctly today -- and it is not this one.
        name: "like and its escape character",
        setup: &[
            "CREATE TABLE L (id INT NOT NULL, s VARCHAR(30) NULL, PRIMARY KEY (id))",
            "INSERT INTO L VALUES (1,'a%'),(2,'ab'),(3,'a_'),(4,'a!'),(5,NULL)",
            // Left empty on purpose. 8.3 settles an ESCAPE clause while
            // resolving the item, so a malformed one is an error over a table
            // with no rows -- and this engine settled it per row, which
            // answered an empty result set instead. A populated table cannot
            // ask that question.
            "CREATE TABLE LE (id INT NOT NULL, s VARCHAR(30) NULL, PRIMARY KEY (id))",
            // The other side of a join, empty for the same reason: a join
            // produces no pair of rows when either side has none, so an ON
            // clause is settled for no row at all.
            "CREATE TABLE LF (id INT NOT NULL, s VARCHAR(30) NULL, PRIMARY KEY (id))",
        ],
        cases: &[
            // The two from the issue. The second is the decisive one: the
            // pattern is `a'%`, and only an engine escaping with `'` can
            // answer 1.
            same("SELECT 'a%' LIKE 'a!%' ESCAPE '!'"),
            same("SELECT 'a%' LIKE 'a''%' ESCAPE '!'"),
            same("SELECT 'a%' NOT LIKE 'a!%' ESCAPE '!', 'a%' NOT LIKE 'a''%' ESCAPE '!'"),
            // The escape before a character that is neither wildcard nor
            // itself: 8.3 drops the escape and matches the character.
            same("SELECT 'ab' LIKE 'a!b' ESCAPE '!', 'a!b' LIKE 'a!b' ESCAPE '!'"),
            // The escape escaping itself.
            same("SELECT 'a!' LIKE 'a!!' ESCAPE '!', 'a!!' LIKE 'a!!!' ESCAPE '!'"),
            // A trailing escape matches itself rather than failing the
            // pattern, which this engine got wrong until #0093 measured it:
            // `'a!' LIKE 'a!' ESCAPE '!'` is 1 and `'a' LIKE 'a!'` is 0.
            same("SELECT 'a!' LIKE 'a!' ESCAPE '!', 'a' LIKE 'a!' ESCAPE '!'"),
            same("SELECT '!' LIKE '!' ESCAPE '!', '' LIKE '!' ESCAPE '!'"),
            same("SELECT 'a!' LIKE '%!' ESCAPE '!', 'ab!' LIKE 'a%!' ESCAPE '!'"),
            // The escape character *is* the wildcard. Escaping wins in every
            // position, the trailing one included.
            same("SELECT 'a%' LIKE 'a%%' ESCAPE '%', 'ab' LIKE 'a%' ESCAPE '%', \
                  'a%' LIKE 'a%' ESCAPE '%'"),
            same("SELECT 'a_' LIKE 'a__' ESCAPE '_', 'ab' LIKE 'a_' ESCAPE '_', \
                  'a_' LIKE 'a_' ESCAPE '_'"),
            // An empty ESCAPE turns escaping off rather than naming a
            // character, so the default backslash goes away with it.
            same("SELECT 'a%' LIKE 'a!%' ESCAPE '', 'a%' LIKE 'a%' ESCAPE ''"),
            same(r"SELECT 'a%' LIKE 'a\\%' ESCAPE '', 'a\\%' LIKE 'a\\%' ESCAPE ''"),
            // ESCAPE NULL is not that: it leaves the default backslash in
            // place, which is the one place NULL does not propagate.
            same(r"SELECT 'a%' LIKE 'a\\%' ESCAPE NULL, 'a%' LIKE 'a!%' ESCAPE NULL"),
            same(r"SELECT 'a\\%' LIKE 'a\\\\%' ESCAPE NULL"),
            // A non-string is converted to one first.
            same("SELECT 'x%' LIKE 'x1%' ESCAPE 1, 'x' LIKE 'x' ESCAPE TRUE, \
                  'x%' LIKE 'x!%' ESCAPE x'21'"),
            // Double quotes are a string here, so this names `!` and not a
            // column. The group runs without ANSI_QUOTES.
            same("SELECT 'a%' LIKE 'a!%' ESCAPE \"!\""),
            // The length is counted in characters and not in bytes, so a
            // two-byte character is one escape character on a utf8mb4
            // connection.
            same("SELECT 'a%' LIKE CONCAT('a', 'é', '%') ESCAPE 'é'"),
            same("SELECT 'aé' LIKE CONCAT('a', 'é', 'é') ESCAPE 'é'"),
            // Longer than one character is ER_WRONG_ARGUMENTS. `1.5` is three
            // characters once converted, which is why a number can raise it.
            same("SELECT 'a%' LIKE 'a!%' ESCAPE '!!'"),
            same("SELECT 'a%' LIKE 'a!%' ESCAPE 'ab'"),
            same("SELECT 'x%' LIKE 'x1%' ESCAPE 1.5"),
            // Raised while resolving and not while evaluating: the operand is
            // NULL and the answer is still the error.
            same("SELECT NULL LIKE 'a' ESCAPE '!!'"),
            same("SELECT NULL LIKE 'a' ESCAPE '!', 'a' LIKE NULL ESCAPE '!'"),
            // The default with no clause at all, which ORMs rely on: they
            // escape user input with a backslash and write no ESCAPE.
            same(r"SELECT 'a_b' LIKE 'a\\_b', 'axb' LIKE 'a\\_b'"),
            same(r"SELECT 'a\\' LIKE 'a\\', 'a' LIKE 'a\\', 'ab' LIKE 'a\\'"),
            // Per row rather than per statement, and through NOT as well: the
            // clause is resolved once and the pattern matched for each.
            same("SELECT id, s LIKE 'a!%' ESCAPE '!', s NOT LIKE 'a!%' ESCAPE '!' \
                  FROM L ORDER BY id"),
            same("SELECT id FROM L WHERE s LIKE 'a!_' ESCAPE '!' ORDER BY id"),
            // A binary operand compares byte for byte, so the pattern's own
            // case stops being negotiable. Bytes against characters is not
            // asked: the operand is explicitly binary and MySQL carries that
            // collation into the answer where this engine has none.
            same("SELECT 'A%' LIKE BINARY 'a!%' ESCAPE '!', 'a%' LIKE BINARY 'a!%' ESCAPE '!'")
                .without_value_kind(),
            // A binary string is measured in bytes, because that is what one
            // of its characters is: `x'C3A9'` is two where the same two bytes
            // written as text are one. `x''` is off, like `''`.
            same("SELECT 'a%' LIKE 'a!%' ESCAPE x'C3A9'"),
            same("SELECT 'a%' LIKE 'a!%' ESCAPE x'2121'"),
            same("SELECT 'a%' LIKE 'a!%' ESCAPE x'', 'a%' LIKE 'a%' ESCAPE x''"),
            // Resolve time against row time. Every one of these is a statement
            // that evaluates the LIKE for no row at all -- an empty table, a
            // filter over one, a sort over one, a branch nothing takes, an AND
            // that short-circuits -- and 8.3 raises 1210 for all of them.
            // Nothing here can be caught by a case that returns a row.
            same("SELECT s LIKE 'a' ESCAPE '!!' FROM LE"),
            same("SELECT id FROM LE WHERE s LIKE 'a' ESCAPE '!!'"),
            same("SELECT id FROM LE ORDER BY s LIKE 'a' ESCAPE '!!'"),
            same("SELECT 1 FROM LE HAVING MAX(s) LIKE 'a' ESCAPE '!!'"),
            // A bare-aggregate HAVING is not the same question as the three
            // below it, and was standing in for them: it runs over one
            // implicit group whatever the table holds, so it is settled by the
            // row path and says nothing about a resolve-time guard. With a
            // GROUP BY over an empty table there are no groups, and the clause
            // is reached by nothing else.
            same("SELECT 1 FROM LE GROUP BY s HAVING MAX(s) LIKE 'a' ESCAPE '!!'"),
            same("SELECT s, COUNT(*) FROM LE GROUP BY s HAVING s LIKE 'a' ESCAPE '!!'"),
            same("SELECT COUNT(*) FROM LE GROUP BY s LIKE 'a' ESCAPE '!!'"),
            // The ON clause, inner and outer. The outer one is asked as well
            // because a LEFT JOIN keeps its left rows without ever matching a
            // pair, so it reaches the clause by a different route.
            same("SELECT a.id FROM LE a JOIN LF b ON a.s LIKE b.s ESCAPE '!!'"),
            same("SELECT a.id FROM LE a LEFT JOIN LF b ON a.s LIKE b.s ESCAPE '!!'"),
            // A write, which is the worse half of the same gap: an illegal
            // statement and a WHERE that matched nothing are both
            // `Affected { rows: 0 }`, so a caller cannot tell them apart.
            same("UPDATE LE SET id = 2 WHERE s LIKE 'a' ESCAPE '!!'"),
            same("DELETE FROM LE WHERE s LIKE 'a' ESCAPE '!!'"),
            same("UPDATE LE SET s = CASE WHEN 0 THEN ('a' LIKE 'b' ESCAPE '!!') ELSE 'z' END"),
            same(
                "UPDATE LE a JOIN LF b ON a.id = b.id SET a.s = 'x' \
                 WHERE a.s LIKE 'a' ESCAPE '!!'",
            ),
            same("DELETE a FROM LE a JOIN LF b ON a.id = b.id WHERE a.s LIKE 'a' ESCAPE '!!'"),
            // A table that does not exist is answered before the escape is:
            // 8.3 is 1146 here, not 1210, so the guard sits after the table is
            // resolved rather than in front of the statement.
            same("DELETE FROM nosuch WHERE s LIKE 'a' ESCAPE '!!'"),
            same("SELECT CASE WHEN 0 THEN ('a' LIKE 'b' ESCAPE '!!') ELSE 9 END"),
            same("SELECT 0 AND ('a' LIKE 'b' ESCAPE '!!')"),
            // A name that resolves against nothing wins: 8.3 resolves names
            // before it settles the escape, so this is 1054 and not 1210.
            same("SELECT nope LIKE 'x' ESCAPE '!!' FROM LE"),
            // The same shape with a legal clause, so the guard above is shown
            // to refuse the malformed one rather than the construct.
            same("SELECT id, s LIKE 'a!%' ESCAPE '!' FROM LE ORDER BY id"),
            same("SELECT s, COUNT(*) FROM LE GROUP BY s HAVING s LIKE 'a!%' ESCAPE '!'"),
            same("SELECT a.id FROM LE a JOIN LF b ON a.s LIKE b.s ESCAPE '!' ORDER BY a.id"),
            same("DELETE FROM LE WHERE s LIKE 'a!%' ESCAPE '!'"),
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
            // `X'41'`, `0x41` and `-'abc'` were held out of this group while
            // they diverged on something that is not a name -- the hex
            // literal's UNSIGNED flag and the negation's type code. Both are
            // answered now, and both have a group of their own below rather
            // than being folded in here, because neither is about naming.
            // #0105 and #0106.
            same("SELECT TRUE, true, +TRUE"),
                        // Bytes against characters not asked: the operand is explicitly
            // binary -- `BINARY 'x'`, `CAST(x AS BINARY)`, a `BLOB` column --
            // and MySQL carries that collation into the answer where this
            // engine has none to carry.
            same("SELECT 'a' = 'a', 'a' IS NULL, CONCAT('a'), BINARY 'a'").without_value_kind(),
            same("SELECT t, 'abc', +'abc' FROM L ORDER BY id"),
            // A run whose first literal is escaped: the doubled quote and the
            // appended literal cancel out in the lengths, so only the text
            // separates a run from a literal that merely looks like one.
            same("SELECT 'a''a' 'b', 'a\\'a' 'b', 'a' 'a''a'"),
            same("SELECT '' 'x', '' '', 'aa' ''"),
            same("SELECT * FROM (SELECT 'abc') d"),
            // These eight read a view whose column is a string literal, and
            // their widths are not asked. MySQL declares such a column from
            // `Item::max_length` — `varchar(2)` for `'ab'` — where this engine
            // has no width for an expression and declares `varchar(512)`. The
            // cases are kept because what they ask is the *name*, which both
            // servers still answer identically.
            same("SELECT * FROM LV").without_widths(),
            same("SELECT * FROM LP").without_widths(),
            same("SELECT * FROM LE").without_widths(),
            // A view over `1 MOD 3` belongs here too and is not here: MySQL
            // materialises that column as INT and this engine as BIGINT,
            // which is about a view's shape rather than about naming. It is
            // covered by name in `crates/engine/tests/sql.rs`.
            same("SELECT * FROM LU ORDER BY 1").without_widths(),
            same("SELECT * FROM LD").without_widths(),
            same("SELECT * FROM LC").without_widths(),
            same("SELECT * FROM LB").without_widths(),
            same("SELECT * FROM LBB").without_widths(),
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
        // The two the group above narrowed rather than blessed, now that both
        // are answered. #0105 and #0106.
        //
        // A hex literal's column is UNSIGNED on 8.3 -- the one place that flag
        // describes something that is not a number -- and its values are
        // bytes. A *bit* literal is neither, which is why the cases below name
        // one spelling: `b'101'` is a VAR_STRING with no UNSIGNED there, and
        // it does not parse here at all, so it cannot be asked (#0141).
        //
        // Unary minus answers a number for every operand MySQL accepts, so
        // `-'abc'` is a DOUBLE and not the VAR_STRING this engine sent. Only
        // the type moved: `-0` is the text either way, which is why the text
        // protocol never showed it and a prepared client read a string where
        // MySQL sends eight bytes of a double.
        //
        // What is deliberately not here is `-X'41'` and `ABS(X'41')`: MySQL
        // reads a hex literal as the *number* 65 in a numeric context and this
        // engine reads its bytes, so those answer -65 against -0 and 65
        // against 0. That is #0141 and not this, and it is why the cases below
        // keep the hex literal in a string context.
        name: "a hex literal and a unary minus on the wire",
        setup: &[
            "CREATE TABLE H (id INT NOT NULL, t VARCHAR(30) NOT NULL, \
             d DECIMAL(10,2) NOT NULL, iu INT UNSIGNED NOT NULL, PRIMARY KEY (id))",
            "INSERT INTO H VALUES (1, '12abc', 1.25, 7)",
        ],
        cases: &[
            // Every spelling of the literal, and both edges of its length.
            same("SELECT X'41', 0x41, x'41'"),
            same("SELECT X'', X'4142434445464748494A4B4C4D4E4F5051525354555657'"),
            // The wrappers MySQL's parser hands the item straight back
            // through, which keep the flag and the character set with it.
            same("SELECT (X'41'), ((X'41')), +X'41'"),
            same("SELECT (SELECT X'41')"),
            // A materialisation takes the flag off, because the temporary
            // table it lands in has a field per column and a field is unsigned
            // only where it is a number. The character set survives.
            same("SELECT X'41' UNION SELECT X'42' ORDER BY 1"),
            same("SELECT * FROM (SELECT X'41' AS h) z"),
            // A string context, where both servers read the bytes.
            same("SELECT LENGTH(X'41'), X'41' IS NULL, X'41' = 0x41"),
            // The negations. A string of any shape is a DOUBLE, an exact
            // fraction stays NEWDECIMAL and an integer of any width and either
            // sign is a LONGLONG.
            same("SELECT -'abc', -'12', -'12abc'"),
            same("SELECT -NULL, -TRUE"),
            same("SELECT -t, -d, -iu FROM H ORDER BY id"),
            // Both branches answer -0, so the union is one row and its order
            // is total without an ORDER BY.
            same("SELECT -'abc' UNION SELECT -'def'"),
            same("SELECT * FROM (SELECT -'abc' AS m) z"),
            // The type reaches the arithmetic around it: a negated decimal
            // still adds exactly, where announcing it approximate would have
            // rounded the answer through an f64.
            same("SELECT -d + 1, -iu + 1 FROM H ORDER BY id"),
        ],
    },
    Group {
        // The other half of the rule above. A literal carries a parse-time
        // name and so does a column reference, and unary plus and parentheses
        // are handed straight back by MySQL's parser rather than recorded — so
        // `SELECT +a` is the column `a`, in the name and in everything else
        // the definition carries. #0117.
        //
        // Measured on 8.3 over `DECIMAL(10,2) NOT NULL`: `+a`, `+(a)`, `+ a`,
        // `(a)` and `+ +a` each come back named `a`, 12 wide, NOT NULL and
        // reporting the table they were read from — the same packet `SELECT a`
        // sends. Unary minus builds a node MySQL records and is named after
        // its text, `- a` keeping the space.
        //
        // The plus is invisible everywhere else the parse is read, too: inside
        // a larger expression it types as its operand where a minus computes,
        // and a position or a select-list name is resolved through it. Both
        // are here, because both were wrong for the same reason.
        name: "unary plus, which the parser discards",
        setup: &[
            "CREATE TABLE Pl (id INT NOT NULL, a DECIMAL(10,2) NOT NULL, n INT NULL, \
             s VARCHAR(20) NULL, f FLOAT NULL, tn TINYINT NULL, u INT UNSIGNED NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO Pl VALUES (1, 1.50, 7, 'hi', 1.5, 3, 7), \
             (2, -2.25, NULL, NULL, 0.5, 1, 2)",
            // An alias that shadows a real column of the same table, which is
            // what makes a peeled `ORDER BY +b` visible: a bare name takes the
            // select list and the same name inside an expression takes the
            // relation, and those are opposite orders over these rows.
            "CREATE TABLE Sh (id INT NULL, b INT NULL)",
            "INSERT INTO Sh VALUES (1, 30), (2, 20), (3, 10)",
            // A view is stored as text and re-run on read, and a CTAS declares
            // a table from the definition: two paths that re-derive the column
            // rather than copy the packet, and both have to lose the plus. 8.3
            // stores the view as ``select `Pl`.`a` AS `a` `` and declares the
            // copy `decimal(10,2) NOT NULL`.
            "CREATE VIEW PlV AS SELECT +a FROM Pl",
            "CREATE TABLE PlCopy AS SELECT +a FROM Pl",
        ],
        cases: &[
            // The shapes from the issue, beside the reference they are.
            same("SELECT a, +a, +(a), + a, (a), + +a, ((+((a)))) FROM Pl ORDER BY id"),
            same("SELECT +Pl.a, +(Pl.a) FROM Pl ORDER BY id"),
            same("SELECT +id, +n, +s FROM Pl ORDER BY id"),
            // An alias wins the name and the rest of the definition is still
            // the column's.
            same("SELECT +a AS x FROM Pl ORDER BY id"),
            // The boundary: unary minus is not peeled, and neither is a plus
            // over anything that is not a reference.
            same("SELECT -a, - a, -(a), +-a, -+a FROM Pl ORDER BY id"),
            same("SELECT +a+1, +(a+1), +ABS(a) FROM Pl ORDER BY id"),
            // The clauses that resolve a reference of their own. `GROUP BY +n`
            // groups by the column on 8.3, so the select list's `n` is a
            // grouped column there rather than a nonaggregated one.
            same("SELECT +a FROM Pl GROUP BY a ORDER BY 1"),
            same("SELECT n FROM Pl GROUP BY +n ORDER BY 1"),
            same("SELECT +a FROM Pl ORDER BY +a"),
            // The paths that build a definition rather than copy one.
            same("SELECT * FROM (SELECT +a FROM Pl) d ORDER BY 1"),
            same("WITH c AS (SELECT +(a) FROM Pl) SELECT * FROM c ORDER BY 1"),
            same("SELECT +a FROM Pl UNION SELECT +a FROM Pl ORDER BY 1"),
            same("SELECT * FROM PlV ORDER BY 1"),
            same(
                "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE FROM information_schema.columns \
                 WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME IN ('PlV', 'PlCopy') \
                 ORDER BY TABLE_NAME",
            ),
            // A plus makes nothing resolvable, and 1054 names the reference
            // rather than the item it was written in.
            same("SELECT +zz FROM Pl"),
            same("SELECT +Pl.zz FROM Pl"),
            // Inside a larger expression the plus types as its operand and
            // the minus computes: over a FLOAT the plus stays a FLOAT and the
            // minus is a DOUBLE, over a TINYINT the plus stays a TINY and the
            // minus widens, and over an `INT UNSIGNED` the plus keeps the
            // flag the minus drops. A dropped UNSIGNED reaches a binary
            // client as a negative number.
            same("SELECT NULLIF(+f, 1), NULLIF(f, 1), NULLIF(-f, 1) FROM Pl ORDER BY id"),
            same("SELECT NULLIF(+tn, 1), NULLIF(tn, 1), NULLIF(-tn, 1) FROM Pl ORDER BY id"),
            same(
                "SELECT CASE WHEN 1 THEN +u END, CASE WHEN 1 THEN u END, \
                 CASE WHEN 1 THEN -u END FROM Pl ORDER BY id",
            ),
            same("SELECT +u + 1, u + 1, IF(1, +u, NULL), IF(1, u, NULL) FROM Pl ORDER BY id"),
            // A position is read through the plus and not through a minus,
            // which is a wrong row order rather than an error: `ORDER BY -1`
            // is a constant on 8.3 and sorts nothing.
            //
            // The minus is asked with a second key behind it, because a case
            // whose rows are not totally ordered can differ on the plan rather
            // than on the answer. `id` settles it and the discrimination
            // survives: read as a position, `-1` would sort on `a` and put
            // -2.25 first, and read as the constant it is, `id` decides and
            // 1.50 comes first.
            same("SELECT a FROM Pl ORDER BY +1"),
            same("SELECT a FROM Pl ORDER BY -1, id"),
            same("SELECT a FROM Pl GROUP BY +1 ORDER BY + +1"),
            same("SELECT a FROM Pl ORDER BY +2"),
            // And a select-list name, against an alias shadowing a column:
            // `+b` is the alias on 8.3 and `b + 0` is the column.
            same("SELECT id AS b FROM Sh ORDER BY +b"),
            same("SELECT id AS b FROM Sh ORDER BY b + 0"),
            same("SELECT DISTINCT id AS b FROM Sh ORDER BY +b"),
            same("SELECT DISTINCT id AS b FROM Sh ORDER BY -b"),
            same("SELECT id AS b, COUNT(*) FROM Sh GROUP BY id ORDER BY +b"),
            // A variable belongs beside these and is not here: it parses as an
            // identifier, is not peeled — 8.3 names `SELECT +@v` after its
            // text — and its column is described binary there where this
            // engine has no collation to carry, which is #0128 rather than
            // this. Covered by name in `crates/engine/tests/sql.rs`.
            //
            // So is the grouped `ORDER BY -b`, which both servers refuse with
            // 1055 and describe differently — this engine names the
            // expression where 8.3 names the column. The refusal is asserted
            // by code in `crates/engine/tests/sql.rs`; the message is its own
            // gap and does not belong in a group about the plus.
        ],
    },
    Group {
        name: "bitwise operators",
        setup: &[
            "CREATE TABLE B (id INT NOT NULL, n INT NULL, neg INT NULL, u BIGINT UNSIGNED NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO B VALUES (1, 5, -1, 12), (2, NULL, NULL, NULL), (3, 12, -6, 255)",
            // Written as strings, from when a decimal literal reached a column
            // through an f64 and the digits are the point of the table. #0070
            // made the literal exact, so the quoting no longer carries
            // anything — it is left alone because changing it would change
            // what these cases cover.
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
            // A literal with a point is a DECIMAL to MySQL, and is one here
            // since #0070 — until then it evaluated to an f64 and had to be
            // read off its token instead, which is the workaround these cases
            // now hold the removal of. An exponent is a DOUBLE to MySQL and
            // still reads as one.
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
            // hands the field straight back — so `+d` is `d` itself, named
            // `d`, typed DATE and 10 wide. It carried an alias and had its
            // widths excused for as long as this engine computed a new column
            // for it; #0117 is why neither is needed now.
            same("SELECT d - 0, d * 1, d MOD 7, d DIV 1, -d, +d FROM Tm ORDER BY id"),
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
            // Bytes against characters not asked: `MAX(d)` and `MIN(tm)` go
            // through a temporary table, and a temporal that has is reported
            // in the connection's character set on 8.3 rather than as binary.
            same("SELECT SUM(d), AVG(d), MAX(d), MIN(tm) FROM Tm")
                .without_value_kind(),
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
        // A SET column, which the corpus had none of at all until #0087 — so
        // the half of that issue about SET_FLAG had nothing to prove it, and
        // the engine's SET width was the one number in #0126 that was
        // measured and then never asked again.
        //
        // The point of the group is the *description*: MySQL sends a SET as
        // `MYSQL_TYPE_STRING`, the code a `CHAR(n)` also goes out under, with
        // SET_FLAG and a width of every member and its commas. So each case
        // reads the column through one more thing that could lose the
        // declaration — a derived table, a view, a CTE, a grouping, and a set
        // operation, which is the one that is *supposed* to lose it.
        name: "a set column",
        setup: &[
            // Members out of alphabetical order, so the declared order and the
            // sorted one are different lists, and one of three characters, so
            // the width is not the member count.
            "CREATE TABLE S (id INT NOT NULL, s SET('b','a','ccc') NULL, \
             sn SET('a','b') NOT NULL, PRIMARY KEY (id))",
            // A row holding two members, a NULL, and the empty set — the three
            // values a SET has that a string column does not.
            "INSERT INTO S VALUES (1, 'a', 'a'), (2, 'b,a', 'a,b'), (3, NULL, '')",
            "CREATE VIEW SV AS SELECT id, s FROM S",
        ],
        cases: &[
            same("SELECT id, s, sn FROM S ORDER BY id"),
            // Everything that reads the column back out of a table keeps the
            // declaration on 8.3, and reports the table it came from.
            same("SELECT id, s FROM (SELECT id, s FROM S) d ORDER BY id"),
            same("WITH c AS (SELECT id, s FROM S) SELECT id, s FROM c ORDER BY id"),
            same("SELECT id, s FROM SV ORDER BY id"),
            same("SELECT s, COUNT(*) FROM S GROUP BY s ORDER BY s"),
            same("SELECT DISTINCT s FROM S ORDER BY s"),
            // A set operation materialises the column, and the flag goes with
            // it: 8.3 sends this one as a VAR_STRING with no flags, where
            // every case above it is a STRING with SET_FLAG.
            same("SELECT id, s FROM S UNION ALL SELECT id, s FROM S ORDER BY id, s"),
            // An expression over the column is an item and not a field, so
            // there is nothing for the flag to be read off.
            same(
                "SELECT id, CONCAT(s, ''), LENGTH(s), s = 'a', s LIKE '%a%' \
                 FROM S ORDER BY id",
            ),
            same("SELECT MIN(s), MAX(s), COUNT(s) FROM S"),
            // A member list is a member list wherever it is compared: an
            // equality against the whole set matches and against one of its
            // members does not.
            same("SELECT id, sn FROM S WHERE sn = 'a,b' ORDER BY id"),
            // What a schema describer reads, which is the other half of the
            // description: `COLUMN_TYPE` carries the members in their declared
            // order, and `CHARACTER_MAXIMUM_LENGTH` the width in characters.
            same(
                "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, \
                 CHARACTER_MAXIMUM_LENGTH FROM information_schema.columns \
                 WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'S' \
                 ORDER BY ORDINAL_POSITION",
            ),
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
        // `BETWEEN` is two comparisons joined by `AND`, and the `AND` is what
        // decides a NULL bound: a single false side settles the conjunction,
        // so a NULL bound only makes the answer unknown when the *other* side
        // is true. A WHERE clause cannot ask any of it, because a NULL and a 0
        // both reject a row, so the cases are projections, aggregates and
        // wrappers — with one WHERE at the end to hold the filtering the rest
        // of them cannot see.
        name: "between and NULL",
        setup: &[
            "CREATE TABLE Btw (id INT NOT NULL, s BIGINT NULL, lo BIGINT NULL, hi BIGINT NULL, \
             d DATE NULL, dlo DATE NULL, m DECIMAL(10,2) NULL, mhi DECIMAL(10,2) NULL, \
             txt VARCHAR(10) NULL, PRIMARY KEY (id))",
            "INSERT INTO Btw VALUES \
             (1, 3, 4, NULL, '2020-01-01', '2021-01-01', 3.50, 2.50, 'a'), \
             (2, 3, 2, NULL, '2022-01-01', '2021-01-01', 1.50, 2.50, 'c'), \
             (3, 5, NULL, 4, '2020-01-01', '2021-01-01', 3.50, 2.50, 'a'), \
             (4, 3, NULL, 4, '2022-01-01', '2021-01-01', 1.50, 2.50, 'c'), \
             (5, NULL, 2, 1, '2020-01-01', '2021-01-01', 3.50, 2.50, NULL)",
        ],
        cases: &[
            // The four that separate the truth table from "any NULL bound
            // makes it NULL": the first two are 0 and the last two are NULL.
            same("SELECT 3 BETWEEN 4 AND NULL, 5 BETWEEN NULL AND 4, \
                  3 BETWEEN NULL AND 4, 3 BETWEEN 2 AND NULL"),
            // The bound is inclusive, so an equal one is the true side and
            // leaves the answer unknown rather than settling it.
            same("SELECT 3 BETWEEN 3 AND NULL, 4 BETWEEN NULL AND 4, 3 BETWEEN NULL AND NULL"),
            // A NULL subject is unknown against both bounds however they are
            // placed, an empty range included.
            same("SELECT NULL BETWEEN 1 AND 2, NULL BETWEEN 2 AND 1, NULL BETWEEN NULL AND NULL"),
            // `NOT BETWEEN` negates the decided rows only — it is not the
            // complement of the table above.
            same("SELECT 3 NOT BETWEEN 4 AND NULL, 5 NOT BETWEEN NULL AND 4, \
                  3 NOT BETWEEN NULL AND 4, NULL NOT BETWEEN 1 AND 2"),
            // Which side is false is the comparison's business, so the same
            // rule runs under each family of them. Strings collate, and
            // `'10' >= '9'` is false where the numbers say the opposite.
            same("SELECT 'a' BETWEEN 'b' AND NULL, 'b' BETWEEN 'a' AND NULL, \
                  'c' BETWEEN NULL AND 'b', 'a' BETWEEN NULL AND 'b'"),
            same("SELECT '10' BETWEEN '9' AND NULL, 10 BETWEEN 9 AND NULL, \
                  '10' BETWEEN NULL AND '9'"),
            same("SELECT 1.5 BETWEEN 2.5 AND NULL, 1.5 BETWEEN 0.5 AND NULL, \
                  3.5 BETWEEN NULL AND 2.5, 1.5 BETWEEN NULL AND 2.5"),
            same("SELECT '3' BETWEEN 4 AND NULL, 3 BETWEEN '4' AND NULL"),
            // Off the literals, so it is evaluated per row rather than folded
            // once, and over a DATE and a DECIMAL as well as an integer.
            same("SELECT id, s BETWEEN lo AND hi, s NOT BETWEEN lo AND hi FROM Btw ORDER BY id"),
            same("SELECT id, d BETWEEN dlo AND NULL, m BETWEEN NULL AND mhi, \
                  txt BETWEEN 'b' AND NULL FROM Btw ORDER BY id"),
            // An aggregate tells 0 from NULL without projecting either: COUNT
            // counts the false rows and skips the unknown ones.
            same("SELECT COUNT(s BETWEEN lo AND hi), SUM(s NOT BETWEEN lo AND hi) FROM Btw"),
            // The wrappers that read the value back, and the document reading,
            // where NULL and false are two different tokens.
            same("SELECT (3 BETWEEN 4 AND NULL) IS NULL, IF(3 BETWEEN 4 AND NULL, 'y', 'n'), \
                  CASE WHEN 3 BETWEEN 4 AND NULL THEN 'y' ELSE 'n' END, \
                  COALESCE(3 BETWEEN 4 AND NULL, 9)"),
            same("SELECT JSON_ARRAY(3 BETWEEN 4 AND NULL, 3 BETWEEN NULL AND 4, \
                  3 NOT BETWEEN 4 AND NULL)"),
            // And the clause that cannot see any of it, kept so the fix is
            // held to leaving filtering alone.
            same("SELECT id FROM Btw WHERE s BETWEEN lo AND hi ORDER BY id"),
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
        // The rewriter changes the statement under both of these modes, and a
        // column named after the *rewrite* is named after text the client
        // never sent. Every case here is a name, not a value: the rows agreed
        // all along.
        name: "sql_mode: ANSI_QUOTES",
        setup: &[
            "SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')",
            "CREATE TABLE Aq (id INT NOT NULL, x INT NULL, `a\"b` INT NULL, \
             `a``b` INT NULL, PRIMARY KEY (id))",
            "INSERT INTO Aq VALUES (1, 5, 6, 7)",
        ],
        cases: &[
            // The rewrite to backticks is the same length here, which is the
            // coincidence that makes a name from the wrong offsets look right.
            same("SELECT \"x\" + 1 FROM Aq"),
            same("SELECT 1 + \"x\" * 2 FROM Aq"),
            // And here it is not: `\"\"` collapses to one byte, so the rewrite
            // is shorter than what the client wrote.
            same("SELECT \"a\"\"b\" + 1 FROM Aq"),
            // A backtick doubles, so this one is longer.
            same("SELECT \"a`b\" + 1 FROM Aq"),
            same("SELECT \"a\"\"b\" + \"a`b\" FROM Aq"),
            // A bare identifier is named by what it denotes rather than by
            // how it was spelled, on both servers.
            same("SELECT \"x\", \"a\"\"b\" FROM Aq"),
        ],
    },
    Group {
        name: "sql_mode: IGNORE_SPACE",
        setup: &[
            "SET SESSION sql_mode = CONCAT(@@sql_mode, ',IGNORE_SPACE')",
            "CREATE TABLE Ig (id INT NOT NULL, n INT NULL, PRIMARY KEY (id))",
            "INSERT INTO Ig VALUES (1, 8)",
        ],
        cases: &[
            // The gap the parser is not allowed to see, which MySQL keeps in
            // the name character for character.
            same("SELECT COUNT (*) FROM Ig"),
            same("SELECT ABS (-1)"),
            same("SELECT CONCAT ('a' , 'b')"),
            same("SELECT LEFT ('abcdef', 3)"),
            // A comment stands where the gap does and is kept just as
            // faithfully.
            same("SELECT ABS /* c */ (-3)"),
            // The call spelling of MOD with a gap, beside the operator
            // spelling: two rewrites of the same word in one statement.
            same("SELECT MOD (n, 3), n MOD 3 FROM Ig"),
            // Nested, and one gap inside the parentheses of another: the
            // removals sit inside the name rather than at its edges.
            same("SELECT ABS (ABS (-1)), 1 + ABS (-1), (COUNT (*)) FROM Ig"),
            // A parenthesised item written *first*, which is the one position
            // where the gap this mode closes is the one after `SELECT`. In the
            // line above the same `(COUNT (*))` comes third, behind a comma —
            // no gap is closed there, so that column cannot see the bug and
            // the group read green while these were wrong.
            same("SELECT (COUNT (*)) FROM Ig"),
            same("SELECT (ABS (-1)) FROM Ig"),
            same("SELECT (n MOD 3) FROM Ig"),
            same("SELECT ( 1 + 2 )"),
            same("SELECT (CONCAT ('a' , 'b'))"),
            // Already right without the walk being widened, because a binary
            // operator's span starts at the `(` rather than inside it. Kept so
            // the two spellings are compared side by side.
            same("SELECT (1) + 2"),
        ],
    },
    Group {
        // Both modes at once, so a name has to survive two passes that change
        // the text by different amounts in different directions.
        name: "sql_mode: ANSI_QUOTES and IGNORE_SPACE",
        setup: &[
            "SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES,IGNORE_SPACE')",
            "CREATE TABLE Mixed (id INT NOT NULL, x INT NULL, `a\"b` INT NULL, \
             `a``b` INT NULL, PRIMARY KEY (id))",
            "INSERT INTO Mixed VALUES (1, 5, 6, 7)",
        ],
        cases: &[
            same("SELECT ABS (\"a`b\"), \"x\" MOD 3, LENGTH (\"a\"\"b\") FROM Mixed"),
            same("SELECT UPPER (\"a\"\"b\"), \"a`b\" mod 2 FROM Mixed"),
            same("SELECT MOD (\"x\", 3) FROM Mixed"),
            // Every pass at once, behind a closed gap after `SELECT`.
            same("SELECT (\"x\" MOD 3) FROM Mixed"),
            same("SELECT (LENGTH (\"a\"\"b\")) FROM Mixed"),
            same("SELECT COUNT (*) FROM Mixed"),
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
        // #0113. `div_precision_increment` decides two things: a quotient's
        // scale, which is its left operand's plus the increment, and an
        // average's, which is the scale it averaged plus the same. 0 is the
        // setting that separates the two steps MySQL takes — it computes to a
        // whole number of nine-digit words and truncates there before rounding
        // to the answer's scale, and at 0 over two integers there is no word
        // at all, so `7/2` is 3 rather than 4 and `AVG(i)` over 1, 2 and 2 is
        // 1 rather than 2.
        //
        // No `SELECT @@div_precision_increment` case in any of these groups:
        // an *overridden* system variable comes back from this engine as a
        // string where MySQL sends an UNSIGNED LONGLONG, which is true of
        // every variable a session sets — #0095 — and `reset` sets this one on
        // every connection, so there is no group where it is not overridden.
        //
        // Every statement in these three groups is aliased to a name of its
        // own, and that is load-bearing rather than decoration. MySQL reads
        // `div_precision_increment` when a statement is *prepared* and fixes
        // the quotient's and the average's scale then; a later `SET` does not
        // re-fix an already-prepared statement, and the client caches a
        // prepared statement by its text across groups. Measured on 8.3 over
        // one connection: prepared at 4 and re-executed after
        // `SET SESSION div_precision_increment = 0`,
        // `SELECT @@div_precision_increment, 1/3, AVG(i) FROM Quot` answers
        // 0, 0.3333, 1.6667 through the binary protocol — the variable current
        // and the arithmetic frozen — against 0, 0, 1 through the text one.
        //
        // Two of these groups sharing one statement's text would therefore ask
        // MySQL about *the first group's* increment. It is
        // only visible for a statement over literals: one that names a table
        // is re-prepared when `reset` replaces the database under it, which is
        // why the `AVG` cases agreed while the literal quotient did not.
        //
        // This engine re-parses from the statement text on every execution and
        // so always answers at the increment in force, which is the divergence
        // — and modelling MySQL's prepare-time fixing for this one variable
        // while `sql_mode` and everything else still re-derive would make the
        // gap depend on which variable was changed. Distinct text per group is
        // what these groups mean to ask: each one's statements are prepared
        // under its own increment.
        name: "div_precision_increment: 0",
        setup: &[
            "CREATE TABLE Quot (
                 id INT NOT NULL,
                 i INT NOT NULL,
                 d DECIMAL(10,2) NOT NULL,
                 w DECIMAL(10,5) NOT NULL,
                 PRIMARY KEY (id)
             )",
            "INSERT INTO Quot (id, i, d, w) VALUES
                 (1, 1, 1.50, 1.00001), (2, 2, 2.50, 0.00000), (3, 2, 3.00, 0.00000)",
            "SET SESSION div_precision_increment = 0",
        ],
        cases: &[
            same("SELECT 1/3 AS zero, 1.5/1.5, 7/2, 1.00001/3"),
            same("SELECT AVG(i) AS zero, AVG(d), AVG(w) FROM Quot"),
            // Unaffected at every setting, measured, which is why they ride
            // along in each group rather than being asked once at the default.
            same("SELECT 7 DIV 2 AS zero, 10.5 MOD 3, MOD(10.5, 3), i DIV 2, d MOD 2 \
                  FROM Quot ORDER BY id"),
            same("SELECT d / i AS zero, w / i, i / d FROM Quot ORDER BY id"),
        ],
    },
    Group {
        // 6 and 30, the second being the top of the range and the one place
        // the 30-place cap on a quotient's scale bites: 1.5 has a place of its
        // own, so 1 + 30 would be 31 and MySQL answers 30.
        name: "div_precision_increment: 6 and 30",
        setup: &[
            "CREATE TABLE Quot (
                 id INT NOT NULL,
                 i INT NOT NULL,
                 d DECIMAL(10,2) NOT NULL,
                 w DECIMAL(10,5) NOT NULL,
                 PRIMARY KEY (id)
             )",
            "INSERT INTO Quot (id, i, d, w) VALUES
                 (1, 1, 1.50, 1.00001), (2, 2, 2.50, 0.00000), (3, 2, 3.00, 0.00000)",
            "SET SESSION div_precision_increment = 6",
        ],
        cases: &[
            same("SELECT 1/3 AS six, 1.5/1.5, 7/2, 1.00001/3"),
            same("SELECT AVG(i) AS six, AVG(d), AVG(w) FROM Quot"),
            same("SELECT 7 DIV 2 AS six, 10.5 MOD 3, MOD(10.5, 3), i DIV 2, d MOD 2 \
                  FROM Quot ORDER BY id"),
            // A case rather than setup, so the group's own increment survives
            // it and both settings are asked in one group.
            same("SET SESSION div_precision_increment = 30"),
            same("SELECT 1/3 AS wide, 1.5/1.5, 7/2, 1.00001/3"),
            same("SELECT AVG(i) AS wide, AVG(d), AVG(w) FROM Quot"),
            same("SELECT 7 DIV 2 AS wide, 10.5 MOD 3, MOD(10.5, 3), i DIV 2, d MOD 2 \
                  FROM Quot ORDER BY id"),
        ],
    },
    Group {
        // The default and the range. The increment is left at what `reset`
        // puts it back to, which is MySQL's documented 4, so the first two
        // cases are the stock answer — and `AVG(w)` is where the truncation to
        // a nine-digit word shows at that default, because the answer's own
        // scale is 9 and nothing rounds the interim down: 0.333336666, not the
        // 0.333336667 that rounding the true quotient gives.
        name: "div_precision_increment: the default and the range",
        setup: &[
            "CREATE TABLE Quot (
                 id INT NOT NULL,
                 i INT NOT NULL,
                 d DECIMAL(10,2) NOT NULL,
                 w DECIMAL(10,5) NOT NULL,
                 PRIMARY KEY (id)
             )",
            "INSERT INTO Quot (id, i, d, w) VALUES
                 (1, 1, 1.50, 1.00001), (2, 2, 2.50, 0.00000), (3, 2, 3.00, 0.00000)",
        ],
        cases: &[
            same("SELECT 1/3 AS four, 1.5/1.5, 7/2, 1.00001/3"),
            same("SELECT AVG(i) AS four, AVG(d), AVG(w) FROM Quot"),
            // Out of range is clamped rather than refused, at both ends.
            same("SET SESSION div_precision_increment = 31"),
            same("SELECT 1/3 AS above, AVG(w) FROM Quot"),
            same("SET SESSION div_precision_increment = -1"),
            same("SELECT 1/3 AS below, AVG(w) FROM Quot"),
            // Above `i64::MAX`, which is a literal anyone can write and which
            // arrives as an unsigned value. Read through a signed `i64` it
            // wraps to -1 and clamps to 0, the far end of the range from the
            // 30 MySQL stores.
            same("SET SESSION div_precision_increment = 18446744073709551615"),
            same("SELECT 1/3 AS huge, AVG(w) FROM Quot"),
            // A value that is not an integer is 1232 on both, a string
            // spelling one included.
            same("SET SESSION div_precision_increment = 'abc'"),
            same("SET SESSION div_precision_increment = '3'"),
            same("SET SESSION div_precision_increment = 3.7"),
            same("SET SESSION div_precision_increment = NULL"),
            // And an integer-valued expression, and a keyword, are taken as
            // the numbers they are.
            same("SET SESSION div_precision_increment = 1 + 1"),
            same("SELECT 1/3 AS computed, AVG(w) FROM Quot"),
            same("SET SESSION div_precision_increment = TRUE"),
            same("SELECT 1/3 AS keyword, AVG(w) FROM Quot"),
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
            // For `ON DUPLICATE KEY UPDATE`, whose version of "no rows" is
            // "nothing collided": `Dup` is empty and `NoKey` has no unique
            // key at all, so nothing written below can ever reach the clause
            // by evaluating it. `Gen` is here for the one ordering that needs
            // a generated column.
            "CREATE TABLE Dup (id INT NOT NULL, k INT NULL, s VARCHAR(20) NULL, \
             PRIMARY KEY (id))",
            "CREATE TABLE NoKey (a INT NULL, b INT NULL)",
            "CREATE TABLE Gen (id INT NOT NULL, k INT NULL, g INT AS (k + 1), \
             PRIMARY KEY (id))",
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
            // Compared and selected. Only the first of these was here while a
            // projected scalar subquery was described VAR_STRING (#0104); the
            // second is the one that carries the subquery's answer out to the
            // client, under the type the client decodes it by.
            same("SELECT S.k, (SELECT COUNT(*) FROM T WHERE T.sid = S.id) > 0 AS has \
                  FROM S ORDER BY S.id"),
            same("SELECT S.k, (SELECT COUNT(*) FROM T WHERE T.sid = S.id) AS n \
                  FROM S ORDER BY S.id"),
            same("SELECT S.k, (SELECT MAX(t.sid) FROM T t WHERE t.id = S.id) AS m \
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

            // #0102. The same hole seen from a write, and the worse half of
            // it: an UPDATE and a DELETE resolved no name at all, so over an
            // empty table every one of these reported nought rows affected —
            // which is exactly what a WHERE that matched nothing reports, so
            // the caller could not tell it from an illegal statement.
            same("UPDATE Bare SET k = 1 WHERE a.b = 1"),
            same("UPDATE Bare SET k = 1 WHERE nope = 1"),
            same("DELETE FROM Bare WHERE a.b = 1"),
            same("DELETE FROM Bare WHERE nope = 1"),
            // The assigned value, which is where the issue's own example
            // sits: nothing evaluates it either.
            same("UPDATE Bare SET k = a.b"),
            // A table with rows in it, refused for the other three reasons a
            // clause goes unevaluated — a filter nothing survives, a LIMIT of
            // none, and a CASE branch nothing takes.
            same("UPDATE S SET k = 1 WHERE 1 = 0 AND a.b = 1"),
            same("DELETE FROM S WHERE 1 = 0 AND a.b = 1"),
            same("UPDATE S SET k = 1 WHERE a.b = 1 LIMIT 0"),
            same("UPDATE S SET k = CASE WHEN 0 THEN a.b ELSE k END"),
            // The joined forms, which reach a different pair of entry points.
            // A join with an empty side produces no pair of rows at all.
            same("UPDATE S a JOIN T b ON a.id = b.id SET a.k = 1 WHERE zz.q = 1"),
            same("UPDATE S a JOIN T b ON a.id = b.id SET a.k = zz.q"),
            same("DELETE a FROM S a JOIN T b ON a.id = b.id WHERE zz.q = 1"),
            same("DELETE FROM a USING S a JOIN T b ON a.id = b.id WHERE zz.q = 1"),
            same("UPDATE Bare a JOIN T b ON a.id = b.id SET a.k = 1 WHERE zz.q = 1"),
            same("DELETE a FROM Bare a JOIN T b ON a.id = b.id WHERE zz.q = 1"),
            // An alias replaces the table name on a write too, so the table's
            // own name no longer resolves.
            same("UPDATE S a JOIN T b ON a.id = b.id SET a.k = 1 WHERE S.k = 1"),
            // The orderings, which are what makes these worth more than a
            // refusal each: every one is a different *code* on 8.3 and the
            // engine had to be measured against it rather than reasoned at.
            // A missing table first, then the name, then the malformed
            // ESCAPE — and for a multi-table DELETE, which tables it names
            // before any of them.
            same("UPDATE nosuch SET k = 1 WHERE a.b = 1"),
            same("UPDATE T SET label = 'x' WHERE nope = 1 AND label LIKE 'a' ESCAPE '!!'"),
            same("DELETE a, a FROM S a JOIN T b ON a.id = b.id WHERE nope = 1"),
            // Within a SET list the first assignment's problem wins, exactly
            // as the first select item's does: these two differ only in the
            // order of the assignments and 8.3 answers a different code for
            // each.
            same("UPDATE T SET label = ('a' LIKE 'b' ESCAPE '!!'), sid = nope"),
            same("UPDATE T SET sid = nope, label = ('a' LIKE 'b' ESCAPE '!!')"),

            // Legal from here down, over the empty table so each affects
            // nothing and the group's own rows are left alone. A correlated
            // reference resolves against the row being written rather than
            // against the subquery's FROM, and a back-quoted qualifier is a
            // spelling rather than a name — the two shapes #0058's first
            // resolver wrongly refused.
            same("UPDATE Bare SET k = 1 WHERE id IN (SELECT sid FROM T WHERE T.id = Bare.id)"),
            same("UPDATE Bare SET k = 1 WHERE EXISTS (SELECT 1 FROM T t WHERE t.sid = Bare.id)"),
            same("UPDATE Bare SET k = (SELECT MAX(T.sid) FROM T WHERE T.id = Bare.id)"),
            same("UPDATE Bare b SET b.k = 1 WHERE b.id = 1"),
            same("UPDATE `Bare` SET `k` = 1 WHERE `Bare`.`id` = 1"),
            same("UPDATE differential.Bare SET k = 1 WHERE differential.Bare.id = 1"),
            same("DELETE FROM Bare WHERE `Bare`.`id` = 1"),
            // A bare keyword that parses as an identifier, in a predicate
            // that has to keep resolving.
            same("UPDATE Bare SET k = 1 WHERE TIMESTAMPDIFF(DAY, '2026-01-01', '2026-03-05') > 1"),
            // `DEFAULT` parses as an identifier, so a resolver that does not
            // know the keyword reads it as an unknown column and refuses a
            // statement 8.3 accepts. Over the empty table, because the
            // evaluator has no meaning for it either.
            same("UPDATE Bare SET k = DEFAULT"),
            same("UPDATE Bare a JOIN T b ON a.id = b.id SET a.k = DEFAULT"),

            // #0165. The fifth entry point with the same hole, and the one
            // #0102 left out because its scope is different: the clause is
            // read only for a row that collides, so on an empty table, against
            // a key that misses, and on a table with no unique key at all it
            // was not read — every one of these inserted the row and reported
            // success where 8.3 raises 1054.
            same("INSERT INTO Dup (id, k) VALUES (1, 1) ON DUPLICATE KEY UPDATE k = a.b"),
            same("INSERT INTO Dup (id, k) VALUES (1, 1) ON DUPLICATE KEY UPDATE k = nope"),
            same("INSERT INTO Dup (id, k) VALUES (1, 1) ON DUPLICATE KEY UPDATE k = nodb.Dup.k"),
            // `VALUES(x)` names a column of the target table, which is the
            // measurement the scope turns on — so an unknown one is 1054 the
            // same way a bare name is.
            same("INSERT INTO Dup (id, k) VALUES (1, 1) ON DUPLICATE KEY UPDATE k = VALUES(nope)"),
            same("INSERT INTO Dup (id, k) VALUES (1, 1) ON DUPLICATE KEY UPDATE k = VALUES(a.b)"),
            // A collision that cannot happen, and a branch that is not taken:
            // the rest of #0009's checklist as this clause spells it.
            same("INSERT INTO NoKey (a) VALUES (1) ON DUPLICATE KEY UPDATE b = nope"),
            same(
                "INSERT INTO Dup (id, k) VALUES (1, 1) ON DUPLICATE KEY UPDATE \
                 k = CASE WHEN 0 THEN nope ELSE 1 END",
            ),
            // The orderings, each a different code on 8.3: the clause sits
            // behind four of the insert's own checks. 1582 comes out of 8.3's
            // parser so it beats even a missing table; a malformed ESCAPE does
            // not, which is the pair that had to be measured rather than
            // reasoned at.
            same("INSERT INTO nosuch (k) VALUES (1) ON DUPLICATE KEY UPDATE k = nope"),
            same("INSERT INTO Dup (nope2) VALUES (1) ON DUPLICATE KEY UPDATE k = nope"),
            same("INSERT INTO Dup (id, k) VALUES (1) ON DUPLICATE KEY UPDATE k = nope"),
            same("INSERT INTO Gen (g) VALUES (1) ON DUPLICATE KEY UPDATE k = nope"),
            same("INSERT INTO Dup (id, k) VALUES (1, 1), (2) ON DUPLICATE KEY UPDATE k = nope"),
            same("INSERT INTO Gen VALUES (1, 1, 1) ON DUPLICATE KEY UPDATE k = nope"),
            same("INSERT INTO nosuch (k) VALUES (1) ON DUPLICATE KEY UPDATE k = SIGN(1, 2)"),
            same(
                "INSERT INTO nosuch (k) VALUES (1) ON DUPLICATE KEY UPDATE \
                 k = ('a' LIKE 'b' ESCAPE '!!')",
            ),
            // Every target resolves before any value here, which is the
            // opposite of the order the same pair settles in for an UPDATE's
            // SET — and among the values the name and the ESCAPE interleave by
            // position, as they do everywhere else.
            same(
                "INSERT INTO Dup (id, k) VALUES (1, 1) ON DUPLICATE KEY UPDATE \
                 nope1 = ('a' LIKE 'b' ESCAPE '!!')",
            ),
            same(
                "INSERT INTO Dup (id, k) VALUES (1, 1) ON DUPLICATE KEY UPDATE \
                 k = ('a' LIKE 'b' ESCAPE '!!'), s = nope",
            ),
            same(
                "INSERT INTO Dup (id, k) VALUES (1, 1) ON DUPLICATE KEY UPDATE \
                 k = nope, s = ('a' LIKE 'b' ESCAPE '!!')",
            ),
            // Legal, and on `NoKey` so that the second of a case's two
            // protocol runs still cannot collide: an upsert that fired on the
            // second run would be comparing the affected-count rule rather
            // than the resolution. A column the insert did not name, the
            // target table by every spelling of its name, and the bare
            // `DEFAULT` keyword that #0102's review caught being read as a
            // column.
            same("INSERT INTO NoKey (a) VALUES (1) ON DUPLICATE KEY UPDATE b = a"),
            same("INSERT INTO NoKey (a) VALUES (1) ON DUPLICATE KEY UPDATE b = VALUES(a)"),
            same("INSERT INTO NoKey (a) VALUES (1) ON DUPLICATE KEY UPDATE b = NoKey.a"),
            same(
                "INSERT INTO NoKey (a) VALUES (1) ON DUPLICATE KEY UPDATE \
                 b = differential.NoKey.a",
            ),
            same("INSERT INTO NoKey (a) VALUES (1) ON DUPLICATE KEY UPDATE NoKey.b = 1"),
            same("INSERT INTO `NoKey` (`a`) VALUES (1) ON DUPLICATE KEY UPDATE `b` = `NoKey`.`a`"),
            same("INSERT INTO NoKey (a) VALUES (1) ON DUPLICATE KEY UPDATE b = DEFAULT"),
            same(
                "INSERT INTO NoKey (a) VALUES (1) ON DUPLICATE KEY UPDATE \
                 b = TIMESTAMPDIFF(DAY, '2026-01-01', '2026-03-05')",
            ),
            // The two shapes whose scope is *wider* than the target table on
            // 8.3 and which this engine does not model: an `INSERT ... SELECT`
            // also sees the SELECT's FROM tables, and the row alias names the
            // row being inserted. Both are here because resolving them against
            // the table alone would refuse SQL 8.3 answers, which is the
            // direction that matters.
            same("INSERT INTO NoKey (a) SELECT id FROM S x ON DUPLICATE KEY UPDATE b = x.id"),
            same("INSERT INTO NoKey (a) VALUES (1) AS new ON DUPLICATE KEY UPDATE b = new.a"),
            // Last, and the reason the writes above are safe to have here: a
            // refusal that nonetheless wrote would show up as a changed row
            // rather than as nothing at all.
            same("SELECT id, k FROM S ORDER BY id"),
            same("SELECT id, sid, label FROM T ORDER BY id"),
            same("SELECT id, k FROM Bare ORDER BY id"),
            same("SELECT id, k, s FROM Dup ORDER BY id"),
            same("SELECT a, b FROM NoKey ORDER BY a, b"),
            same("SELECT id, k, g FROM Gen ORDER BY id"),
        ],
    },
    Group {
        // #0099. 1054 names the clause the reference was written in, and this
        // engine reported every one of them as the field list because the
        // evaluator carried no clause. GROUP BY said `'field list'` where 8.3
        // says `'group statement'`, a join condition where it says
        // `'on clause'`, and an OVER spec's two interior clauses have names of
        // their own.
        //
        // What the harness can hold of that is the code and the SQLSTATE:
        // `Answer::Failed` records those two and not the message, so a wrong
        // clause name would compare equal here. The message itself is pinned
        // by `crates/engine/tests/sql.rs`, statement for statement against the
        // same measurements. These cases hold the other half — that each of
        // these clauses is refused at all, and refused with 1054 rather than
        // by answering — which is the half a message assertion cannot see.
        //
        // Every table has rows in it, because that is what makes the clause
        // evaluate. Refusing one *before* the first row is #0100's, and a case
        // over an empty table would be recording that divergence rather than
        // this one.
        name: "the clause a 1054 names",
        setup: &[
            "CREATE TABLE Cl (id INT NOT NULL, k INT NULL, s VARCHAR(20) NULL, \
             PRIMARY KEY (id))",
            "CREATE TABLE Cr (id INT NOT NULL, k INT NULL, PRIMARY KEY (id))",
            "INSERT INTO Cl VALUES (1, 10, 'a'), (2, 20, 'b')",
            "INSERT INTO Cr VALUES (1, 10), (2, 20)",
        ],
        cases: &[
            // The two positions #0058 already settled, asked here as well:
            // the clause names come from one mechanism now, so a change that
            // moves it has to keep these two right along with the rest.
            same("SELECT zz.q FROM Cl"),
            same("SELECT k FROM Cl WHERE zz.q = 1"),
            // The six the issue lists.
            same("SELECT k FROM Cl GROUP BY zz.q"),
            same("SELECT k FROM Cl GROUP BY k HAVING zz.q > 1"),
            same("SELECT k FROM Cl ORDER BY zz.q"),
            same("SELECT Cl.k FROM Cl JOIN Cr ON zz.q = Cr.id"),
            same("SELECT SUM(k) OVER (PARTITION BY zz.q) FROM Cl"),
            same("SELECT SUM(k) OVER (ORDER BY zz.q) FROM Cl"),
            // A LEFT join names the same clause as an inner one, and a `USING`
            // list names `'from clause'` rather than the `'on clause'` of the
            // condition the engine builds from it.
            same("SELECT Cl.k FROM Cl LEFT JOIN Cr ON zz.q = Cr.id"),
            same("SELECT Cl.k FROM Cl JOIN Cr USING (zz)"),
            // A DML ORDER BY, which #0100's note measured as `'order clause'`.
            // The WHERE picks a row, so the clause is reached.
            same("UPDATE Cl SET k = 1 WHERE id = 1 ORDER BY zz.q LIMIT 1"),
            same("DELETE FROM Cl WHERE id = 1 ORDER BY zz.q LIMIT 1"),
            // Inside a call and inside an operator: the position decides the
            // name, not the shape of the expression sitting in it.
            same("SELECT k FROM Cl GROUP BY ABS(zz.q)"),
            same("SELECT Cl.k FROM Cl JOIN Cr ON ABS(zz.q) = Cr.id"),
            same("SELECT SUM(k) OVER (PARTITION BY ABS(zz.q)) FROM Cl"),
            same("SELECT SUM(k) OVER (ORDER BY ABS(zz.q)) FROM Cl"),
            same("SELECT k FROM Cl ORDER BY zz.q + 1"),
            // A window function's argument belongs to the clause the call is
            // written in, not to the OVER spec beside it.
            same("SELECT SUM(zz.q) OVER (ORDER BY k) FROM Cl"),
            // A subquery's own clause is named, and so is the clause a
            // correlated reference to the outer query is *written* in — the
            // name follows the position, not the query level that owns the
            // scope.
            same("SELECT k FROM Cl WHERE id IN (SELECT id FROM Cr GROUP BY zz.q)"),
            same("SELECT k FROM Cl WHERE id IN \
                  (SELECT Cr.id FROM Cr JOIN Cl c2 ON zz.q = c2.id)"),
            same("SELECT k FROM Cl WHERE Cl.id IN (SELECT Cr.id FROM Cr GROUP BY Cl.zz)"),
            same("SELECT k FROM Cl WHERE Cl.id IN \
                  (SELECT Cr.id FROM Cr JOIN Cl c2 ON Cl.zz = c2.id)"),
            // Unqualified, which is the evaluator's other arm and the other of
            // the two places this message is spelled.
            same("SELECT k FROM Cl GROUP BY zz"),
            same("SELECT Cl.k FROM Cl JOIN Cr ON zz = Cr.id"),
            same("SELECT SUM(k) OVER (PARTITION BY zz) FROM Cl"),
            same("SELECT SUM(k) OVER (ORDER BY zz) FROM Cl"),
            same("UPDATE Cl SET k = 1 WHERE id = 1 ORDER BY zz LIMIT 1"),
            same("DELETE FROM Cl WHERE id = 1 ORDER BY zz LIMIT 1"),
            // Which clause is named only matters once two of them are bad at
            // the same time, and then it is decided by which one is resolved
            // first. 8.3 reads the select list before the rest of the query
            // body; this engine grouped before it read the list, which nothing
            // could see while every clause reported the same name. Review
            // caught it, and these are the shapes it caught it with.
            same("SELECT zz.q FROM Cl GROUP BY yy.r"),
            same("SELECT zz.q FROM Cl GROUP BY zz.q"),
            same("SELECT zz.q FROM Cl GROUP BY 1"),
            same("SELECT zz.q AS a FROM Cl GROUP BY a"),
            same("SELECT SUM(zz.q) FROM Cl GROUP BY k"),
            // The WHERE is behind the select list too, and ahead of the
            // GROUP BY.
            same("SELECT zz.q FROM Cl WHERE yy.r = 1"),
            same("SELECT zz.q FROM Cl WHERE yy.r = 1 ORDER BY xx.p"),
            same("SELECT k FROM Cl WHERE zz.q = 1 GROUP BY yy.r"),
            // And where the select list is fine, the GROUP BY beats what
            // follows it.
            same("SELECT k FROM Cl GROUP BY yy.r HAVING xx.p > 1"),
            same("SELECT k FROM Cl GROUP BY yy.r ORDER BY xx.p"),
            // The select list carries more than its names, and all of it is
            // settled ahead of the WHERE and the GROUP BY: 1210 and 1582
            // rather than the other clause's 1054, and 1051 for a wildcard
            // qualifier that names nothing.
            same("SELECT s LIKE 'a' ESCAPE '!!' FROM Cl GROUP BY zz.q"),
            same("SELECT SIGN() FROM Cl GROUP BY zz.q"),
            same("SELECT nope.* FROM Cl GROUP BY k"),
            same("SELECT s LIKE 'a' ESCAPE '!!' FROM Cl WHERE nope = 1"),
            same("SELECT SIGN() FROM Cl WHERE nope = 1"),
            same("SELECT nope.* FROM Cl WHERE zz.q = 1"),
            // Every case above is a refusal, so the rows are still there — and
            // a group of nothing but refusals reads the same whether its setup
            // ran or failed on both servers, which is the failure the README
            // warns about.
            same("SELECT id, k, s FROM Cl ORDER BY id"),
        ],
    },
    Group {
        // #0070 and #0085. Arithmetic over a decimal went through an `f64` and
        // came back with whatever the float printed, so the scale MySQL
        // computes was lost at every operator: `a + b` over two columns
        // holding 1.5 was 3 for 3.000, and `1 / 3` was 0.3333333333333333 for
        // 0.3333.
        //
        // Every scale below was measured on 8.3 rather than derived. The rules
        // are not one rule: `+`, `-` and `%` take the wider of the two scales,
        // `*` adds them, `/` adds `div_precision_increment` to the left one,
        // `DIV` has none, and a sign keeps whatever it was given. Only `*` and
        // `/` clamp at 30 places, which is why the over-long literal is here.
        name: "decimal arithmetic",
        setup: &[
            "CREATE TABLE Dm (id INT NOT NULL, two DECIMAL(10,2) NULL, \
             three DECIMAL(8,3) NULL, whole DECIMAL(10,0) NULL, \
             wide DECIMAL(65,30) NULL, big DECIMAL(65,0) NULL, \
             tiny DECIMAL(65,20) NULL, n INT NULL, u BIGINT UNSIGNED NULL, \
             f FLOAT NULL, g DOUBLE NULL, dt DATE NULL, dt3 DATETIME(3) NULL, \
             PRIMARY KEY (id))",
            // Written as decimal literals, not as strings: a literal with a
            // point is a DECIMAL to MySQL, and whether one arrives in a column
            // with its digits intact is part of what these cases hold.
            "INSERT INTO Dm VALUES (1, 1.50, 1.500, 3, 1.5, \
             99999999999999999999999999999999999, 0.00000000005, 3, \
             18446744073709551615, 1.5, 1.5, '2024-01-05', \
             '2024-01-05 06:07:08.5')",
            "INSERT INTO Dm VALUES (2, -2.25, 0.125, 0, 0.000000000000001, 12345, \
             0.00000000001, -7, 2, 0.5, 0.5, '2026-01-02', \
             '2026-01-02 03:04:05.250')",
            "INSERT INTO Dm (id) VALUES (3)",
            // Two 38-digit values one apart, which is past both an `f64`'s
            // seventeen significant digits and an `i128`'s count of them.
            "CREATE TABLE Wd (id INT NOT NULL, a DECIMAL(65,0) NULL, \
             b DECIMAL(65,0) NULL, s DECIMAL(30,10) NULL, PRIMARY KEY (id))",
            "INSERT INTO Wd VALUES (1, 10000000000000000000000000000000000001, \
             10000000000000000000000000000000000000, 2.5000000000)",
            "INSERT INTO Wd VALUES (2, -10000000000000000000000000000000000001, \
             -10000000000000000000000000000000000000, -2.5000000000)",
            "INSERT INTO Wd VALUES (3, 0, 0, 0.0000000000)",
            "CREATE TABLE Sq (id INT NOT NULL, n INT NULL, v DECIMAL(10,2) NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO Sq VALUES (1, 3, 1.50), (2, 7, 2.25)",
        ],
        cases: &[
            same("SELECT id, two, three, whole, wide, big, tiny FROM Dm ORDER BY id"),
            // The issue's own table, and each operator in both operand orders:
            // a rule that reads the *left* scale has to be asked twice.
            same("SELECT two + three, three + two, two - three, three - two \
                  FROM Dm ORDER BY id"),
            same("SELECT two * three, three * two, two / three, three / two \
                  FROM Dm ORDER BY id"),
            same("SELECT two + 1, 1 + two, two * 2, 2 * two, two + 0.5, 0.5 + two \
                  FROM Dm ORDER BY id"),
            same("SELECT -two, -three, -whole, -wide FROM Dm ORDER BY id"),
            // Unary plus is not a sign at all — MySQL's parser discards it —
            // so these are the four columns themselves, each keeping the
            // width and the scale it was declared with. #0117; before it, this
            // engine computed a new column for each and could not be asked.
            same("SELECT +two, +three, +whole, +wide FROM Dm ORDER BY id"),
            same("SELECT ABS(-1.50), ABS(two), ABS(three), ABS(-0.00) FROM Dm ORDER BY id"),
            // `%` and `MOD` are one operator spelled two ways and take the
            // wider scale, padding to it: `10.5 MOD 3.00` is 1.50.
            same("SELECT two % three, two MOD three, three MOD two, whole MOD three \
                  FROM Dm ORDER BY id"),
            same("SELECT 10.5 MOD 3, -10.5 MOD 3, 10.5 MOD -3, -10.5 MOD -3, \
                  10.5 % 3.25"),
            same("SELECT 10.5 MOD 3.00, 3.00 MOD 1, 0.0 MOD 3, 1 MOD 0.5, 4 MOD 2.0, \
                  100 MOD 7"),
            // `DIV` truncates toward zero and answers a BIGINT whatever it
            // divided, so it is the one operator here that can refuse.
            same("SELECT two DIV three, three DIV two, whole DIV 2, -whole DIV 2 \
                  FROM Dm ORDER BY id"),
            same("SELECT 1.5 DIV 0.4, -7.5 DIV 2, 7.5 DIV -2, 7.9 DIV 2"),
            same("SELECT 9223372036854775807.5 DIV 1, -9223372036854775808.0 DIV 1"),
            same("SELECT 9223372036854775808.5 DIV 1"),
            same("SELECT 99999999999999999999999.0 DIV 1"),
            same("SELECT big DIV 7 FROM Dm ORDER BY id"),
            // A zero divisor is NULL for all three, not an error.
            same("SELECT two / 0, two MOD 0, two DIV 0, 0 / two, 0 MOD two \
                  FROM Dm ORDER BY id"),
            // #0085: division is four places wider than its left operand even
            // over two plain integers, and the quotient is computed to a whole
            // number of nine-digit words and truncated there before being
            // rounded to the result's scale. `1.00001 / 3` is where the two
            // steps separate — 0.333336666, not 0.333336667.
            same("SELECT 1/3, 10/3, 2/3, 1/7, 7/2, 1/8, 5/8, 1/16"),
            same("SELECT 1.00001/3, 1.0/3, 1.00/3, 1.000/3, 1.0000/3, 1.00000/3, \
                  1.000000/3"),
            same("SELECT 1.0000000000000000000000000/7"),
            same("SELECT -1/3, -2/3, 1/-3, -1/-3, -1/8, -3/8"),
            same("SELECT SUM(two) / COUNT(*), AVG(two), SUM(two) + 0 FROM Dm"),
            // A quotient is covered by what it *prints*. MySQL's own value
            // carries more places than the column declares — `JSON_ARRAY(1/3)`
            // is `[0.333333333]` against a `decimal(5,4)` column — so anything
            // that reads a quotient instead of printing one still diverges.
            // That is #0118, and it needs a scale on the column before it can
            // be fixed rather than moved.

            // Past what an `i128` counts, which is 38 digits: MySQL's DECIMAL
            // is 65 and a product of two of them is 70, printed in full.
            same("SELECT wide + wide, wide * wide, wide / wide, wide - wide \
                  FROM Dm ORDER BY id"),
            same("SELECT big * big, big + big, big - big FROM Dm ORDER BY id"),
            same("SELECT tiny * tiny, tiny + tiny, tiny / tiny FROM Dm ORDER BY id"),
            same("SELECT wide * tiny, tiny / wide, wide MOD tiny FROM Dm ORDER BY id"),
            // Exact beside an integer and beside a BIGINT UNSIGNED, whose
            // range no signed integer holds; approximate beside a FLOAT or a
            // DOUBLE, which is where MySQL stops answering exactly.
            same("SELECT two + n, two * n, two / n, n / two, n + three FROM Dm ORDER BY id"),
            same("SELECT two + u, two * u, two / u, u / 2, u + 1.5 FROM Dm ORDER BY id"),
            // `DIV` keeps the UNSIGNED tag from either operand, and a decimal
            // divisor must not take it off: `u DIV 1.0` is
            // 18446744073709551615 on 8.3 exactly as `u DIV 1` is, and
            // answering the first 1690 would make the answer depend on how
            // the 1 was written.
            same("SELECT u DIV 1.0, u DIV 2.0, u MOD 1.5, u MOD 3 FROM Dm ORDER BY id"),
            same("SELECT u DIV three, u MOD three, u / three FROM Dm ORDER BY id"),
            same("SELECT 18446744073709551615 DIV 1.0, 18446744073709551615 DIV 2.0, \
                  18446744073709551615 MOD 1.0"),
            // De-duplication keys a decimal by its number rather than by its
            // text, at every width: two spellings of one 70-digit product are
            // one row.
            same("SELECT COUNT(DISTINCT v) FROM (SELECT two AS v FROM Dm \
                  UNION ALL SELECT two * 1.0 FROM Dm) t"),
            same("SELECT COUNT(*) FROM (SELECT big * big AS v FROM Dm \
                  UNION SELECT big * big * 1.0 FROM Dm) t"),
            same("SELECT two + f, two * f, two / f, two + g, two / g FROM Dm ORDER BY id"),
            // A temporal packs to a number and computes exactly with one.
            // `-dt3` is left out: MySQL types that DOUBLE and still prints its
            // three declared places, which is a float-formatting gap.
            same("SELECT two + dt, two * dt, two / dt, dt / 1, dt + 0 FROM Dm ORDER BY id"),
            same("SELECT two + dt3, two / dt3, dt3 / 1, dt3 + 0 FROM Dm ORDER BY id"),
            // A literal keeps every place it was written and drops the zeros
            // in front, and one past both integer types is a DECIMAL rather
            // than a rounded double.
            same("SELECT 0.5, 100.0, 1.50, .5, 007.5, 0.0, 00.00, -0.5"),
            same("SELECT 0.1 + 0.2, 0.5 + 0.5, 1.50 * 2, 2.675 * 1"),
            same("SELECT 18446744073709551615, 18446744073709551616, \
                  99999999999999999999999"),
            same("SELECT 18446744073709551616 + 1, 18446744073709551616 * 2, \
                  18446744073709551616 / 2"),
            // An exponent is a DOUBLE to MySQL, so it stays approximate and
            // makes what it touches approximate too.
            same("SELECT 1.5e0, 1.5e3, 1.5e0 + 1, 1.5e0 / 2, 1.5 + 1.5e0"),
            // 37 places, which only a literal can carry: `+`, `-`, `%` and a
            // sign keep all of them and `*` and `/` clamp at 30.
            same("SELECT 1.5000000000000000000000000000000000000 + 0, \
                  1.5000000000000000000000000000000000000 * 1"),
            same("SELECT 1.5000000000000000000000000000000000000 / 1, \
                  1.5000000000000000000000000000000000000 MOD 1"),
            same("SELECT -1.5000000000000000000000000000000000000, \
                  ABS(-1.5000000000000000000000000000000000000)"),
            // NULL comes from the columns rather than from a literal: MySQL
            // types *any* arithmetic with a NULL literal in it DOUBLE — `1 +
            // NULL`, `two + NULL` and `-NULL` are all DOUBLE on 8.3, and only
            // `NULL DIV 2` is not — where this engine types it by the other
            // operand. That is a rule about NULL and not about decimals, and
            // it is filed rather than fixed here.
            same("SELECT two + n, two - n, two * n, two / n, -n FROM Dm WHERE id = 3"),
            same("SELECT two + three + whole, (two + three) * two, two * three / two \
                  FROM Dm ORDER BY id"),
            same("SELECT id FROM Dm WHERE two / three = 1.000000 ORDER BY id"),
            same("SELECT CAST('2.5' AS DECIMAL(10,1)) / 2, \
                  CAST('2.5' AS DECIMAL(10,1)) * 3"),
            // A subquery on either side of `/`. Its type cannot be worked out
            // ahead of running it, and "cannot type" is not "approximate": a
            // quotient whose operands are not *known* to be approximate is a
            // NEWDECIMAL. Announced DOUBLE, the exact answer was parsed back
            // through an `f64` on its way out, so `(SELECT 1)/2` printed 0.5
            // as a column and 0.5000 inside a `CONCAT` — one expression, two
            // answers.
            same("SELECT (SELECT 1)/2, 2/(SELECT 2), (SELECT 1)/(SELECT 2), \
                  CONCAT((SELECT 1)/2)"),
            same("SELECT (SELECT n FROM Sq WHERE id = 1)/2, \
                  (SELECT v FROM Sq WHERE id = 1)/2 FROM Sq ORDER BY id"),
            same("SELECT (SELECT SUM(n) FROM Sq)/2, (SELECT SUM(v) FROM Sq)/2"),
            same("SELECT n/(SELECT 2), v/(SELECT n FROM Sq WHERE id = 1) \
                  FROM Sq ORDER BY id"),
            // Comparison has to be as exact as the arithmetic beside it. Read
            // through an `f64` these two columns compared equal while `a - b`
            // answered 1 — the same row saying two things — and `=` disagreed
            // with the `GROUP BY` key, which was already exact.
            same("SELECT a, b, a - b, a = b, a > b, a < b, a >= b, b > a \
                  FROM Wd ORDER BY id"),
            same("SELECT id FROM Wd WHERE a = b ORDER BY id"),
            same("SELECT id FROM Wd WHERE a > b ORDER BY id"),
            same("SELECT a FROM Wd ORDER BY a"),
            same("SELECT COUNT(DISTINCT a) FROM (SELECT a FROM Wd \
                  UNION ALL SELECT b FROM Wd) t"),
            same("SELECT 18446744073709551616 = 18446744073709551615, \
                  18446744073709551616 > 18446744073709551615"),
            same("SELECT 10000000000000000000000000000000000001 = \
                  10000000000000000000000000000000000000"),
            // And every spelling of one number is still one value, which the
            // exact path has to keep: a sign on a zero is not a difference.
            same("SELECT 1.50 = 1.5, 1.50 > 1.5, -0.00 = 0, 2.50 < 10.00, \
                  -2.5 < -1.5, 0.0 = -0.0"),
            same("SELECT s = 2.5, s = 2.50000, s > 2.4999999999, \
                  s BETWEEN 2 AND 3 FROM Wd ORDER BY id"),
        ],
    },
    Group {
        // A table built out of a query's result. The engine had no CTAS at
        // all: it ignored the query and made a table with no columns, which
        // reported success and then desynchronised the connection on the next
        // statement, because a zero column count is byte-for-byte an OK
        // packet. #0125.
        //
        // The metadata is the point here rather than the rows. MySQL declares
        // the table from the result's own description, so what it keeps and
        // what it drops -- a copied column's DEFAULT survives, its
        // AUTO_INCREMENT and its keys do not -- is only visible through
        // `information_schema`, and `SHOW CREATE TABLE`, which is where it is
        // written on 8.3, is not a statement this engine answers.
        //
        // What this group deliberately cannot ask: MySQL narrows a computed
        // column to the width its expression can produce, so `AS SELECT 1`
        // declares `int` where this engine declares `bigint`, and
        // `AS SELECT 'x'` declares `varchar(1)` against `varchar(512)`. That
        // width is `Item::max_length`, which the engine does not model at all
        // -- #0126 -- so a case for it would be a case for that issue and not
        // for this one.
        name: "a table built from a query",
        setup: &[
            "CREATE TABLE Src (id INT NOT NULL AUTO_INCREMENT, \
             name VARCHAR(40) NOT NULL DEFAULT 'x', qty INT UNSIGNED NULL DEFAULT 7, \
             note TEXT NULL, amt DECIMAL(10,2) NULL, \
             PRIMARY KEY (id), UNIQUE KEY uk_name (name))",
            "INSERT INTO Src (name, qty, note, amt) VALUES \
             ('a', 1, 'n1', 1.50), ('b', 2, 'n2', 2.50)",
            "CREATE TABLE Star AS SELECT * FROM Src",
            "CREATE TABLE Expr AS SELECT id + 1 AS a, name FROM Src",
            "CREATE TABLE Nul AS SELECT NULL AS a",
            "CREATE TABLE Nada AS SELECT id FROM Src WHERE 0",
            "CREATE TABLE Ex1 (z BIGINT, y VARCHAR(5)) AS SELECT id, name FROM Src",
            "CREATE TABLE Ex2 (name VARCHAR(5)) AS SELECT id, name FROM Src",
            "CREATE TABLE Copy LIKE Src",
            "CREATE TABLE One (a INT)",
            "CREATE TABLE Two (a INT, b INT)",
            // An AUTO_INCREMENT column the query does not fill, which has to
            // be assigned as if the rows had been inserted rather than taking
            // the type's implicit default.
            "CREATE TABLE Auto (seq INT NOT NULL AUTO_INCREMENT PRIMARY KEY) \
             AS SELECT id FROM Src",
            // An alias renames the result column and leaves the underlying one
            // alone, so the declaration has to follow the source column and
            // not the name. `Src.name` is a NOT NULL VARCHAR with a default.
            "CREATE TABLE Alias AS SELECT id AS name FROM Src",
            // Two columns of one type and different defaults, so following the
            // name rather than the source is visible even where the types
            // agree.
            "CREATE TABLE Pair (a INT DEFAULT 1, b INT DEFAULT 2)",
            "INSERT INTO Pair VALUES (7, 8)",
            "CREATE TABLE Renamed AS SELECT a AS b FROM Pair",
            // A table alias, and a table alias that shadows the name of a real
            // table with a column of the same name and type. The default has
            // to follow the table the alias stands for -- `Src`, whose `name`
            // defaults to 'x' -- and not the name the query wrote, or this
            // copies 'z' out of a table the statement never read.
            "CREATE TABLE Shadow (name VARCHAR(40) DEFAULT 'z')",
            "CREATE TABLE Aliased AS SELECT s.name FROM Src s",
            "CREATE TABLE Shadowed AS SELECT Shadow.name FROM Src Shadow",
        ],
        cases: &[
            // The count a CTAS reports is the rows it inserted. Asked as a
            // case rather than in setup because that count is half the answer;
            // the second protocol asks it again and gets 1050 from both
            // servers, which is the other half.
            same("CREATE TABLE Star2 AS SELECT * FROM Src"),
            same("CREATE TABLE Nada2 AS SELECT id FROM Src WHERE 0"),
            same("CREATE TABLE Copy2 LIKE Src"),
            // A copied column keeps its type, its nullability and its DEFAULT
            // and loses its AUTO_INCREMENT and its keys.
            same("SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE, COLUMN_KEY, \
                  EXTRA FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Star' \
                  ORDER BY ORDINAL_POSITION"),
            same("SELECT * FROM Star ORDER BY id"),
            // An expression is typed by what it computes, and is NOT NULL
            // where its operand is -- which is where the implicit DEFAULT 0
            // comes from, since a plain CREATE TABLE would leave it none.
            same("SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE \
                  FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Expr' \
                  ORDER BY ORDINAL_POSITION"),
            // Widths not asked: `a` is `id + 1`, and MySQL declares the
            // column it builds from that expression 12 wide — `Item::max_length`
            // again — where this engine declares a plain `bigint`. The group's
            // own comment above records the same gap for the type.
            same("SELECT * FROM Expr ORDER BY a").without_widths(),
            // A default copied through an alias, and through one that shadows
            // a real table: both are `'x'` out of `Src` on 8.3.
            same("SELECT TABLE_NAME, COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE \
                  FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME IN ('Aliased', 'Shadowed') \
                  ORDER BY TABLE_NAME"),
            // Two columns of one name is not a table, and the plain spelling
            // is refused as the `AS SELECT` one always was.
            same("CREATE TABLE Dupe (a INT, a INT)"),
            same("CREATE TABLE DupeCase (a INT, A INT)"),
            same("CREATE TABLE DupeKey (a INT, a INT, PRIMARY KEY (nope))"),
            // An all-NULL column is binary(0) -- the statement #0073 settled
            // that measurement with, now that the engine can answer it.
            same("SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE \
                  FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Nul' \
                  ORDER BY ORDINAL_POSITION"),
            same("SELECT * FROM Nul"),
            // An empty result still declares the table.
            same("SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE \
                  FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Nada' \
                  ORDER BY ORDINAL_POSITION"),
            same("SELECT COUNT(*) FROM Nada"),
            // Explicit definitions are merged with the query's columns rather
            // than replacing them: one the query does not name keeps its place
            // in front, one it does names that column in the query's position.
            same("SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE \
                  FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Ex1' \
                  ORDER BY ORDINAL_POSITION"),
            same("SELECT * FROM Ex1 ORDER BY id"),
            same("SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE \
                  FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Ex2' \
                  ORDER BY ORDINAL_POSITION"),
            same("SELECT * FROM Ex2 ORDER BY id"),
            // LIKE is the opposite of AS SELECT in what it keeps: the whole
            // definition, including the AUTO_INCREMENT and both keys, and none
            // of the rows.
            same("SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE, COLUMN_KEY, \
                  EXTRA FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Copy' \
                  ORDER BY ORDINAL_POSITION"),
            same("SELECT INDEX_NAME, COLUMN_NAME, NON_UNIQUE FROM information_schema.statistics \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Copy' \
                  ORDER BY INDEX_NAME, SEQ_IN_INDEX"),
            same("SELECT COUNT(*) FROM Copy"),
            // Two columns of one name is not a table, however it arose.
            same("CREATE TABLE D1 AS SELECT id, id FROM Src"),
            same("CREATE TABLE D2 AS SELECT * FROM Src a JOIN Src b ON a.id = b.id"),
            // The query is resolved before the name is checked, so a bad query
            // is reported as one even under IF NOT EXISTS on a name in use.
            same("CREATE TABLE D3 AS SELECT * FROM Nope"),
            same("CREATE TABLE IF NOT EXISTS Src AS SELECT * FROM Nope"),
            same("CREATE TABLE Src AS SELECT 1 AS a"),
            same("CREATE TABLE D4 LIKE Nope"),
            same("CREATE TABLE Src LIKE Src"),
            // The rows go in on the same path INSERT writes by, so every
            // row-level rule the new table declares applies to them. Each of
            // these was accepted before, and the table was born holding rows
            // its own constraints forbid.
            same("CREATE TABLE Dup (k INT PRIMARY KEY) AS SELECT 1 AS k UNION ALL SELECT 1"),
            same("CREATE TABLE Bad (a INT NOT NULL) AS SELECT NULL AS a"),
            same("CREATE TABLE Chk (id INT, CHECK (id > 100)) AS SELECT id FROM Src"),
            // And a refused CTAS leaves no table behind, which is the other
            // half of the claim: the count would be 3 if it did.
            same("SELECT COUNT(*) FROM information_schema.tables \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME IN ('Dup', 'Bad', 'Chk')"),
            // An unfilled AUTO_INCREMENT column counts from one rather than
            // filling every row with the implicit default -- which, in a
            // PRIMARY KEY column, was every row holding 0.
            same("SELECT * FROM Auto ORDER BY seq"),
            same("SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE, COLUMN_KEY, \
                  EXTRA FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Auto' \
                  ORDER BY ORDINAL_POSITION"),
            // An alias takes the source column's attributes under the alias's
            // name. Following the name found a different column and copied a
            // VARCHAR default into an INT.
            same("SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE \
                  FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Alias' \
                  ORDER BY ORDINAL_POSITION"),
            same("SELECT COLUMN_NAME, COLUMN_DEFAULT, IS_NULLABLE, COLUMN_TYPE \
                  FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Renamed' \
                  ORDER BY ORDINAL_POSITION"),
            // The other route to a table with no columns, and the one that
            // does not go through CREATE at all.
            same("ALTER TABLE One DROP COLUMN a"),
            // One ALTER is one statement however many operations it lists, so
            // a refusal must undo the ones before it. This answered 1090 and
            // dropped `a` and its data anyway.
            same("ALTER TABLE Two DROP COLUMN a, DROP COLUMN b"),
            same("SELECT COLUMN_NAME FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Two' \
                  ORDER BY ORDINAL_POSITION"),
        ],
    },
    Group {
        // The group that makes the byte level visible at all.
        //
        // Every other group here compares what a statement *prints*, so two
        // answers that print the same are the same answer. `HEX` is the one
        // function that breaks that tie -- and until it existed no case in
        // this file could ask what any function returns in bytes. #0074's
        // binary-charset bug was found by reading MySQL's source and probing
        // by hand for exactly that reason, and the SOUNDEX cases below are the
        // measurements that issue had to take against MySQL alone.
        //
        // So the point of the group is not `HEX`. It is that everything under
        // "seen through HEX" is a question about another function, asked at a
        // resolution this corpus did not have.
        name: "bytes and the characters that describe them",
        setup: &[
            "CREATE TABLE Byt (id INT NOT NULL, b VARBINARY(20) NULL, v VARCHAR(20) NULL, \
             e ENUM('a','b','c') NULL, s SET('x','y','z') NULL, n INT NULL, \
             d DECIMAL(10,2) NULL, f FLOAT NULL, bu BIGINT UNSIGNED NULL, dt DATE NULL, \
             PRIMARY KEY (id))",
            "INSERT INTO Byt VALUES (1, 'abc', 'abc', 'b', 'x,z', 255, 12.50, 1.5, \
             18446744073709551615, '2024-01-05')",
        ],
        cases: &[
            // -- HEX itself -------------------------------------------------
            //
            // The string branch: the argument's bytes, two uppercase
            // characters each, and a multi-byte character is all of its bytes.
            same("SELECT HEX('abc'), HEX(''), HEX(NULL), HEX('\u{20ac}abc')"),
            same("SELECT HEX(X'C3A9'), HEX(CAST('\u{e9}' AS BINARY))"),
            // The numeric branch, which MySQL picks by the argument's type
            // rather than by what it holds: the 64-bit pattern, so a negative
            // number is its two's complement and zero is a single `0`.
            same("SELECT HEX(255), HEX(0), HEX(-1), HEX(-255), HEX(TRUE)"),
            // A DECIMAL rounds half away from zero on the way in. It is not
            // the rounding a DOUBLE gets -- that one is half to even -- and it
            // is not the truncation `BIN` does to the same values below.
            same("SELECT HEX(1.5), HEX(1.4), HEX(-1.5), HEX(2.5), HEX(-2.5), HEX(0.5)"),
            same("SELECT HEX(18446744073709551615), HEX(9223372036854775807), \
                  HEX(9223372036854775808)"),
            // A DOUBLE rounds half to *even*, which is a different answer from
            // the DECIMAL line above for the same three values: 2.5 goes down
            // here and up there. Nothing in `HEX(1.5)`, `HEX(d)` or `HEX(f)`
            // can tell the two apart — every one of them answers the same
            // under either rule — so this line is the only thing in the corpus
            // that holds the split to MySQL rather than to a unit test's
            // memory of it.
            same("SELECT HEX(0.5e0), HEX(1.5e0), HEX(2.5e0), HEX(3.5e0), HEX(-0.5e0), \
                  HEX(-1.5e0), HEX(-2.5e0)"),
            // And the two directions past the BIGINT range, which also
            // disagree: a DECIMAL saturates silently and a DOUBLE raises 1690.
            same("SELECT HEX(CAST(1e30 AS DECIMAL(35,0))), \
                  HEX(CAST(-1e30 AS DECIMAL(35,0)))"),
            same("SELECT HEX(1e30 + 0)"),
            // Every kind of stored column, and the split is the whole claim: a
            // temporal, an ENUM and a SET take the string branch -- `HEX(dt)`
            // is the hex of `2024-01-05` and not of the packed 20240105 that
            // the same column reads as in arithmetic -- and every number takes
            // the other one.
            same("SELECT HEX(b), HEX(v), HEX(e), HEX(s), HEX(dt) FROM Byt"),
            same("SELECT HEX(n), HEX(d), HEX(f), HEX(bu) FROM Byt"),

            // -- seen through HEX -------------------------------------------
            //
            // #0074's own measurements, which had to be taken against MySQL
            // alone because this statement did not parse here. The premise
            // that issue was filed on -- that SOUNDEX emits a lone lead byte
            // -- is false over utf8mb4, and this is the case that says so.
            same("SELECT HEX(SOUNDEX('\u{20ac}abc')), HEX(SOUNDEX('\u{b5}abc')), \
                  HEX(SOUNDEX('\u{e9}abc'))"),
            // And the bug next door that #0074 did fix: under the binary
            // charset only an ASCII letter counts, so the accent is skipped
            // and the answer starts at the `a`.
            same("SELECT HEX(SOUNDEX(CAST('\u{e9}abc' AS BINARY)))"),
            // Whether a string function measures in characters or in bytes,
            // which prints identically either way and does not hex
            // identically. All four of these count characters.
            same("SELECT HEX(LEFT('\u{20ac}abc', 2)), HEX(RIGHT('\u{20ac}abc', 2)), \
                  HEX(SUBSTRING('\u{20ac}abc', 2)), HEX(REVERSE('\u{20ac}ab'))"),
            same("SELECT HEX(LPAD('\u{e9}', 3, 'x')), HEX(RPAD('\u{e9}', 3, 'x')), \
                  HEX(SPACE(3))"),
            same("SELECT HEX(UPPER('\u{e9}')), HEX(LOWER('\u{c9}')), \
                  HEX(CONCAT('a', '\u{e9}')), HEX(TRIM('  \u{e9}  '))"),

            // -- UNHEX ------------------------------------------------------
            //
            // Bytes on the way out, which is the half of these functions that
            // is a fact about the column rather than about the client: 8.3
            // describes `UNHEX` with the binary character set where `HEX` gets
            // the connection's, and this file compares that on every column.
            same("SELECT UNHEX('4142'), FROM_BASE64('YWJj')"),
            // An odd-length string is read as if a zero nibble were written in
            // front of it, so `UNHEX('a')` is one byte and not NULL.
            same("SELECT HEX(UNHEX('616263')), HEX(UNHEX('61626')), HEX(UNHEX('a')), \
                  HEX(UNHEX('c3a9')), HEX(UNHEX('C3A9'))"),
            // Bytes that are not text at all. A lone 0xFF is not valid UTF-8,
            // so an answer that had been through a lossy decode on the way out
            // would hex as EFBFBD -- which is a divergence nothing in this
            // file could have seen before `HEX` was here to ask.
            same("SELECT HEX(UNHEX('FF')), HEX(UNHEX('C3')), HEX(UNHEX('00FF'))"),
            // Anything that is not hex is NULL rather than skipped -- a space
            // and a minus sign included -- and the empty string is the empty
            // string.
            same("SELECT UNHEX('zz'), UNHEX('6g'), UNHEX('61 62'), UNHEX('-1'), \
                  HEX(UNHEX(''))"),
            // There is no numeric branch at all: the number prints as
            // characters and those characters are read as hex, so `UNHEX(0)`
            // is a NUL byte and `UNHEX(12.5)` is NULL for its point.
            same("SELECT HEX(UNHEX(616263)), HEX(UNHEX(0)), UNHEX(12.5), UNHEX(-1)"),
            same("SELECT HEX(UNHEX(e)), UNHEX(s), HEX(UNHEX(b)) FROM Byt"),
            // Over a table, which the engine plans through a different
            // projection from the bare literal above -- and it is the column
            // definition being compared here, not the two characters.
            same("SELECT UNHEX(v), FROM_BASE64(v), HEX(v) FROM Byt"),
            // And through a group, which is a third projection path again.
            same("SELECT UNHEX(v), FROM_BASE64(v), HEX(v) FROM Byt GROUP BY v"),
            // A derived table and a CTE do not go through a projection path at
            // all: they rebuild the column from its declared type, so they are
            // the one place a column of bytes could turn back into a column of
            // characters while every case above stayed green. It did.
            same("SELECT x, h FROM (SELECT UNHEX('4142') AS x, HEX('ab') AS h) d"),
            same("WITH c AS (SELECT FROM_BASE64('YWJj') AS x, TO_BASE64('ab') AS h) \
                  SELECT x, h FROM c"),
            same("SELECT UNHEX('4142') AS x UNION ALL SELECT UNHEX('4344')"),
            // And the rows through it, at the byte level: a byte that is not
            // UTF-8 has to come out of a derived table as the byte that went
            // in. Nothing but `HEX` can ask that.
            same("SELECT HEX(x) FROM (SELECT UNHEX('FF') AS x) d"),

            // -- BIN --------------------------------------------------------
            //
            // `CONV(N, 10, 2)`, which reads its argument through the
            // characters it prints as. That is not the number the value
            // carries, and three cases here say so: `BIN(12.5)` truncates at
            // the point where `HEX(12.5)` rounded, and `BIN(dt)` is 2024
            // rather than the packed 20240105.
            same("SELECT BIN(12), BIN(0), BIN(1), BIN(-1), BIN(255), BIN(TRUE)"),
            same("SELECT BIN(12.5), BIN(12.4), BIN(-12.5), BIN(18446744073709551615)"),
            // The empty string alone is NULL: a space, and text with no digits
            // in it, are both `0`.
            same("SELECT BIN(''), BIN(' '), BIN('abc'), BIN('12abc'), BIN('abc12'), \
                  BIN('010')"),
            same("SELECT BIN(n), BIN(d), BIN(f), BIN(bu), BIN(v), BIN(e), BIN(s), BIN(dt) \
                  FROM Byt"),
            // The parse stops at the first character that is not a digit, so
            // an exponent is worth one bit and a point is worth none.
            same("SELECT BIN(1e30), BIN(2.5e0), BIN(-2.5e0), BIN('12.5')"),
            // Overflow, where the two directions do *not* mirror each other:
            // past the top of a BIGINT UNSIGNED the answer saturates to
            // sixty-four ones, and past the bottom it is `0` rather than the
            // sixty-four ones the negation of a saturated magnitude would
            // give. The boundary either side of it is here too, because a rule
            // stated only at its extremes is a rule nothing has located.
            same("SELECT BIN('99999999999999999999999999'), \
                  BIN('-99999999999999999999999999'), \
                  BIN('-18446744073709551615'), BIN('-18446744073709551616'), \
                  BIN('-9223372036854775809')"),

            // -- the base64 pair --------------------------------------------
            //
            // The same text-out, bytes-in split `HEX` and `UNHEX` have, over
            // the same string reading of a number: `TO_BASE64(255)` encodes
            // the three characters and not the byte.
            same("SELECT TO_BASE64('abc'), TO_BASE64(''), TO_BASE64(NULL), \
                  TO_BASE64('\u{e9}'), TO_BASE64(X'C3A9')"),
            same("SELECT TO_BASE64(255), TO_BASE64(-1), TO_BASE64(12.5), TO_BASE64(0)"),
            same("SELECT TO_BASE64(b), TO_BASE64(v), TO_BASE64(e), TO_BASE64(s), \
                  TO_BASE64(dt) FROM Byt"),
            // It wraps, which nothing about the name says: a newline after
            // every 76 characters and never a trailing one, so 57 bytes encode
            // to 76 characters flat and 58 bytes to 81 rather than 80.
            same("SELECT TO_BASE64('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'), \
                  TO_BASE64('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')"),
            same("SELECT HEX(TO_BASE64('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'))"),
            // Whitespace anywhere is skipped, which is what makes the round
            // trip survive the wrapping above; everything else is refused with
            // NULL, including a length that is not a multiple of four and an
            // `=` anywhere but the end.
            same("SELECT HEX(FROM_BASE64('YWJj YWJj')), HEX(FROM_BASE64('YQ==')), \
                  HEX(FROM_BASE64('YWI=')), HEX(FROM_BASE64(''))"),
            same("SELECT FROM_BASE64('a'), FROM_BASE64('!!!!'), FROM_BASE64('YWJ'), \
                  FROM_BASE64('YWJj='), FROM_BASE64('Y=Wj'), FROM_BASE64(255)"),
            // A *second* padded group, which is its own question and not one
            // the line above asks: `'YWJj='` fails on its length and `'Y=Wj'`
            // on where the `=` is, where each half of this is well formed on
            // its own. MySQL's decoder has a flag for allowing multiple
            // chunks; `FROM_BASE64` does not pass it, and this is the case
            // that says so rather than a comment claiming it.
            same("SELECT FROM_BASE64('YQ==YQ=='), FROM_BASE64('YQ== YQ=='), \
                  FROM_BASE64('YWI=YWJj'), FROM_BASE64('YQ==YWJj')"),
            same("SELECT HEX(FROM_BASE64(TO_BASE64(b))) FROM Byt"),
        ],
    },
    Group {
        // A comparison's collation is resolved from *both* operands, and the
        // binary character set beats every other one. So a single binary
        // operand makes the whole comparison byte for byte — case-sensitive,
        // and by the bytes rather than by the characters.
        //
        // This engine took its byte-exact branch only when both sides were
        // bytes, so a binary string beside a literal fell through to the
        // case-insensitive collation and `CAST('AB' AS BINARY) = 'ab'`
        // answered 1 against 8.3's 0. #0139, and the reason `UNHEX('4142') =
        // 'ab'` could not become a case when #0130 added the function.
        //
        // Measured rather than reasoned: `COLLATION(CAST('AB' AS BINARY))` is
        // `binary` at coercibility 2 against `utf8mb4_0900_ai_ci` at 4 for a
        // literal, and a hex literal is `binary` at 4 — so coercibility alone
        // does not decide it and the character set does.
        //
        // The group carries the grouping and the unique key beside the
        // operators on purpose. `Value::collation_key` decides GROUP BY and
        // DISTINCT where `Value::compare` decides `WHERE` and a unique index,
        // and a fix to one that does not move the other makes a query's
        // grouping contradict its own filter.
        name: "a binary operand decides the collation",
        setup: &[
            "CREATE TABLE Bcmp (id INT NOT NULL, b VARBINARY(16) NULL, t VARCHAR(16) NULL, \
             raw VARBINARY(8) NULL, nul VARBINARY(8) NULL, PRIMARY KEY (id))",
            "INSERT INTO Bcmp VALUES (1,'AB','ab',UNHEX('FF'),UNHEX('004E554C4C')),\
             (2,'ab','AB',UNHEX('FE'),UNHEX('004E554C4C')),(3,'aB','Zz',UNHEX('FF'),NULL)",
            // A binary string into three numeric columns, which is the other
            // half of "a binary string is read as a number the way text is":
            // an engine that reads it for one column and refuses it for
            // another fails this *setup* rather than a case.
            "CREATE TABLE Bnum (i INT NULL, d DOUBLE NULL, n DECIMAL(6,2) NULL)",
            "INSERT INTO Bnum VALUES (UNHEX('3132'), UNHEX('3132'), UNHEX('3132'))",
            "CREATE TABLE Bwide (id INT NOT NULL, bu BIGINT UNSIGNED NULL, PRIMARY KEY (id))",
            "INSERT INTO Bwide VALUES (1, CAST('18446744073709551615' AS BINARY)), \
             (2, '18446744073709551615')",
        ],
        cases: &[
            // The three shapes the issue names: a cast, a hex literal and a
            // column, each against a literal differing only in case. The
            // matching spelling is asked beside each so the group cannot pass
            // by answering 0 to everything.
            same("SELECT CAST('AB' AS BINARY) = 'ab', CAST('AB' AS BINARY) = 'AB'"),
            same("SELECT X'4142' = 'ab', X'4142' = 'AB'"),
            same("SELECT b = 'ab', b = 'AB', b = t FROM Bcmp ORDER BY id"),
            // Either side, since the rule is about the pair and not about
            // which operand came first.
            same("SELECT 'ab' = CAST('AB' AS BINARY), 'AB' = CAST('AB' AS BINARY)"),
            // The ordering operators, which the collation answered 0 to in
            // both directions because it called the two equal: 'AB' is 0x4142
            // and 'ab' is 0x6162, so the uppercase spelling is the smaller.
            same("SELECT CAST('AB' AS BINARY) < 'ab', CAST('AB' AS BINARY) > 'ab', \
                  CAST('AB' AS BINARY) <> 'ab', CAST('AB' AS BINARY) >= 'AB'"),
            // The null-safe operator goes with `=`, and a NULL operand is
            // still unknown for the ordinary one.
            same("SELECT CAST('AB' AS BINARY) <=> 'ab', CAST('AB' AS BINARY) <=> 'AB', \
                  CAST('AB' AS BINARY) <=> NULL, CAST('AB' AS BINARY) = NULL"),
            // The constructs that compare without an operator.
            same("SELECT CAST('AB' AS BINARY) IN ('ab','zz'), \
                  CAST('AB' AS BINARY) IN ('AB','zz'), \
                  CAST('B' AS BINARY) BETWEEN 'a' AND 'c', \
                  CAST('B' AS BINARY) BETWEEN 'A' AND 'C'"),
            // Both answer a *number* rather than the string they chose,
            // because MySQL builds a binary string out of a `CASE` or a
            // `NULLIF` over a binary operand and this engine builds text —
            // #0128's charset propagation, which is not what these are for.
            // Written as strings the two cases report that gap instead of the
            // comparison, which was measured here before they were rewritten.
            same("SELECT CASE CAST('AB' AS BINARY) WHEN 'ab' THEN 1 ELSE 0 END, \
                  CASE CAST('AB' AS BINARY) WHEN 'AB' THEN 1 ELSE 0 END"),
            same("SELECT NULLIF(CAST('AB' AS BINARY),'ab') IS NULL, \
                  NULLIF(CAST('AB' AS BINARY),'AB') IS NULL"),
            // Two binary strings, which were already byte-exact, and the pair
            // where the bytes really are equal.
            same("SELECT CAST('AB' AS BINARY) = CAST('ab' AS BINARY), \
                  CAST('AB' AS BINARY) = CAST('AB' AS BINARY)"),
            // The functions #0130 added, which are the reason this was found.
            same("SELECT UNHEX('4142') = 'ab', UNHEX('4142') = 'AB', \
                  FROM_BASE64('QUI=') = 'ab', FROM_BASE64('QUI=') = 'AB'"),
            // Bytes that are not text, which every case above avoids by using
            // letters. A value has to equal its own cast — the cast built its
            // bytes through a lossy decode, so a byte that is not UTF-8 came
            // back as the three of U+FFFD and the byte-exact comparison then
            // said the two were different. `HEX` beside it, because the
            // equality alone would pass with both sides mangled.
            same("SELECT CAST(UNHEX('FF') AS BINARY) = UNHEX('FF'), \
                  HEX(CAST(UNHEX('FF') AS BINARY)), HEX(CAST(UNHEX('C3A9') AS BINARY))"),
            same("SELECT UNHEX('FF') = UNHEX('FE'), UNHEX('FF') = UNHEX('FF'), \
                  UNHEX('80') = '80'"),
            // A number on the other side is *not* this rule. MySQL reads a
            // binary string in a numeric context exactly as it reads text, so
            // these compare as numbers; `> 3` is the one that tells the two
            // readings apart, because by the characters '1' comes before '3'.
            same("SELECT CAST('12' AS BINARY) > 3, CAST('1' AS BINARY) = 1, \
                  CAST('1' AS BINARY) = 1.0"),
            same("SELECT i, d, n FROM Bnum"),
            // The full width, which the `i64` reading saturated: a BIGINT
            // UNSIGNED column range-checks itself through `as_exact_int`, so a
            // binary string spelling 18446744073709551615 was *stored* as
            // 9223372036854775807 while the same digits as text stored the
            // number they spell. Both rows come from the setup, so a wrong
            // reading is a wrong stored value here and not a wrong answer.
            same("SELECT bu FROM Bwide ORDER BY id"),
            same("SELECT CAST(CAST('18446744073709551615' AS BINARY) AS UNSIGNED), \
                  CAST('18446744073709551615' AS UNSIGNED)"),
            // Trailing space, which a binary comparison never pads away: 'a'
            // is one byte and 'a ' is two, so they differ and the shorter
            // sorts first. The *text* half of the same question is left out
            // deliberately — `'a' = 'a '` is 1 under this connection, whose
            // negotiated collation is the PAD SPACE utf8mb4_general_ci, and 0
            // under the NO PAD utf8mb4_0900_ai_ci a column would get. That is
            // #0129's per-column collation, not this, and it was measured
            // here: MySQL answers 1 through the harness and 0 through a
            // client that has done `SET NAMES utf8mb4`.
            same("SELECT CAST('a' AS BINARY) = 'a ', CAST('a' AS BINARY) < 'a ', \
                  CAST('a' AS BINARY) = CAST('a ' AS BINARY)"),
            // Which rows a query returns, which is what the divergence
            // actually cost: a filter, a join and a self-join over the mixed
            // pair.
            same("SELECT COUNT(*) FROM Bcmp WHERE b = t"),
            same("SELECT COUNT(*) FROM Bcmp WHERE b = 'ab'"),
            same("SELECT COUNT(*) FROM Bcmp x JOIN Bcmp y ON x.b = y.t"),
            // Grouping, de-duplication and sorting over the binary column,
            // where three byte strings that differ only in case are three
            // groups and the sort is by the bytes: 0x4142, 0x6142, 0x6162.
            same("SELECT b, COUNT(*) FROM Bcmp GROUP BY b ORDER BY b"),
            same("SELECT DISTINCT b FROM Bcmp ORDER BY b"),
            same("SELECT b FROM Bcmp ORDER BY b"),
            // And the text column beside it, which still folds case — the
            // whole point being that the two columns answer differently.
            same("SELECT t, COUNT(*) FROM Bcmp GROUP BY t ORDER BY t"),
            same("SELECT b FROM Bcmp ORDER BY b DESC"),
            // The same three questions over bytes that are not text, which is
            // where the grouping key and the comparison came apart: the key
            // decoded lossily, so 0xFF and 0xFE reached one key while `=`
            // called them different. UNION's de-duplication runs on that key
            // too. `raw` is a second column so these do not have to disturb
            // what every case above counts over `b`.
            same("SELECT HEX(raw), COUNT(*) FROM Bcmp GROUP BY raw ORDER BY raw"),
            same("SELECT COUNT(DISTINCT raw) FROM Bcmp"),
            same("SELECT COUNT(*) FROM (SELECT UNHEX('FF') AS x UNION SELECT UNHEX('FE')) u"),
            // And the one shape that says the escaped key cannot land in the
            // text key space: the byte 0x80 hexes to `80`, which is the key
            // the *string* `'80'` would get.
            same("SELECT COUNT(*) FROM (SELECT UNHEX('80') AS x UNION SELECT '80') u"),
            // Nor in the *unescaped byte* key space, which is the harder half:
            // `UNHEX('00584333')` is valid UTF-8 spelling the escape for the
            // single byte 0xC3, so escaping only the invalid strings put those
            // two in one group while `=` kept them apart.
            same("SELECT UNHEX('C3') = UNHEX('00584333')"),
            same("SELECT COUNT(*) FROM \
                  (SELECT UNHEX('C3') AS x UNION SELECT UNHEX('00584333')) u"),
            // Nor in the NULL sentinel's: `exec.rs` keys SQL NULL as `\0NULL`
            // and `UNHEX('004E554C4C')` spells it, so a NULL row joined the
            // value's group and the group reported itself as holding those
            // bytes. `nul` is a column of its own for the same reason `raw`
            // is.
            same("SELECT HEX(nul), COUNT(*) FROM Bcmp GROUP BY nul ORDER BY nul"),
            same("SELECT COUNT(*) FROM \
                  (SELECT UNHEX('004E554C4C') AS x UNION SELECT NULL) u"),
        ],
    },
    Group {
        // The unique key half of #0139, in a group of its own because it is
        // the only shape here that needs a schema the cases above must not
        // have: a UNIQUE index cannot be added to `Bcmp` without changing
        // what every case over it counts.
        //
        // #0149's original bug was a unique key, and `Catalog::check_unique`
        // reaches `Value::compare` by a different path from `WHERE` — so the
        // rule is asked here directly rather than inferred.
        name: "a unique key over binary data",
        setup: &[
            "CREATE TABLE Ubin (b VARBINARY(16) NOT NULL, UNIQUE KEY (b))",
            "INSERT INTO Ubin VALUES ('AB'),('ab'),('aB')",
            "CREATE TABLE Umix (b VARBINARY(16) NOT NULL, t VARCHAR(16) NOT NULL, \
             UNIQUE KEY (b, t))",
            "INSERT INTO Umix VALUES ('AB','x'),('ab','X')",
        ],
        cases: &[
            // Three rows, not one: the second and third INSERT above are
            // duplicates under the collation and distinct under the bytes, so
            // a wrong answer here fails the *setup* rather than the case.
            same("SELECT COUNT(*) FROM Ubin"),
            same("SELECT b FROM Ubin ORDER BY b"),
            // The mixed key, where the text half still folds case and the two
            // rows are distinct only because the binary half does not.
            same("SELECT COUNT(*) FROM Umix"),
            same("SELECT b, t FROM Umix ORDER BY b"),
        ],
    },
    Group {
        // MySQL's case folding is a per-character table: one character in,
        // exactly one character or itself out, never a sequence. Rust's
        // `to_uppercase` is Unicode's *full* mapping, which expands -- so
        // `UPPER('stra\u{df}e')` was `STRASSE` here and is `STRA\u{df}E` on
        // 8.3, a wrong answer whose `LENGTH` and `CHAR_LENGTH` are wrong with
        // it. #0140.
        //
        // Asked through `HEX` because nothing else can see most of it: a
        // ligature and the character it expands to print differently, but a
        // combining mark does not.
        //
        // **Which table is a property of the collation.** 8.3 has two for
        // utf8mb4 and they disagree on 616 characters upward and 608 downward.
        // A column declared without a `COLLATE` clause gets
        // `utf8mb4_0900_ai_ci`, which is what this engine encodes; a *literal*
        // takes the connection's, and the client here negotiates 45,
        // `utf8mb4_general_ci`. Where the two disagree, this engine can only
        // match one of them, and #0129 is what would let it match both.
        //
        // So which form a character is asked in here is decided per character
        // and by measuring, not by a rule: the ~1200 disagreements are
        // scattered through the BMP and only 193 are above it, so no range
        // describes them. Two classes needed the column form, and they are the
        // only two in this group:
        //
        //   - a character above the BMP, which `general_ci` never folds
        //   - `\u{1e9e}`, whose lowercase is `\u{df}` under one collation and
        //     itself under the other
        //
        // Everything else below was checked against both collations and
        // answers the same under each, so a literal asks what it looks like it
        // asks. Adding a case means running that check, not consulting the
        // classes above.
        name: "case folding is a table, not Unicode",
        setup: &[
            "CREATE TABLE Cf (id INT NOT NULL, t VARCHAR(30) NULL, PRIMARY KEY (id))",
            "INSERT INTO Cf VALUES (1,'stra\u{df}e'),(2,'\u{1f0}\u{fb01}'),(3,'\u{130}'),\
             (4,'\u{10d0}'),(5,'\u{1f80}'),(6,'\u{10428}'),(7,'\u{1e9e}')",
        ],
        cases: &[
            // The issue as filed, and the length that goes with it.
            same("SELECT HEX(UPPER('stra\u{df}e')), LENGTH(UPPER('stra\u{df}e')), \
                  CHAR_LENGTH(UPPER('stra\u{df}e'))"),
            // The rest of the class: a ligature, and the four whose full
            // mapping is a base plus one or two combining marks.
            same("SELECT HEX(UPPER('\u{fb01}')), HEX(UPPER('\u{1f0}')), \
                  HEX(UPPER('\u{149}')), HEX(LOWER('\u{130}'))"),
            same("SELECT HEX(UPPER('\u{1e96}')), HEX(UPPER('\u{390}')), \
                  HEX(UPPER('\u{587}'))"),
            // The opposite exception, and the reason "leave a sequence alone"
            // is not the whole rule: Unicode uppercases the Greek
            // iota-subscript forms to two characters and 8.3 answers the
            // precomposed one.
            same("SELECT HEX(UPPER('\u{1f80}')), HEX(UPPER('\u{1f90}')), \
                  HEX(UPPER('\u{1fa0}'))"),
            same("SELECT HEX(UPPER('\u{1fb3}')), HEX(UPPER('\u{1fc3}')), \
                  HEX(UPPER('\u{1ff3}'))"),
            // Case pairs 8.3's tables do not have, so both halves stay put
            // where Unicode maps them: Georgian's Mtavruli capitals are
            // Unicode 11 and these tables are Unicode 9.
            same("SELECT HEX(UPPER('\u{10d0}')), HEX(LOWER('\u{1c90}')), \
                  HEX(UPPER('\u{a7b9}')), HEX(LOWER('\u{a7b8}'))"),
            // Two tables and not one: neither composition is the identity, so
            // neither direction may be derived from the other.
            same("SELECT HEX(UPPER('\u{b5}')), HEX(LOWER(UPPER('\u{b5}'))), \
                  HEX(LOWER('\u{b5}'))"),
            same("SELECT HEX(UPPER('\u{130}')), HEX(UPPER(LOWER('\u{130}'))), \
                  HEX(LOWER('\u{17f}')), HEX(LOWER(UPPER('\u{17f}')))"),
            same("SELECT HEX(LOWER('\u{212a}')), HEX(UPPER(LOWER('\u{212a}'))), \
                  HEX(LOWER('\u{2126}')), HEX(UPPER(LOWER('\u{2126}')))"),
            // A run of the table whose members are every other character: the
            // capital of each pair must not move under `UPPER`, nor the small
            // one under `LOWER`.
            same("SELECT HEX(UPPER('\u{101}')), HEX(UPPER('\u{100}')), \
                  HEX(LOWER('\u{100}')), HEX(LOWER('\u{101}'))"),
            // The ordinary case, which the table must not have broken.
            same("SELECT HEX(UPPER('ab\u{e9}')), HEX(LOWER('AB\u{c9}')), \
                  HEX(UPPER('\u{ff41}')), HEX(LOWER('\u{ff21}'))"),
            // And once without `HEX`, so the rows a client would actually see
            // are compared too, with `UCASE` and `LCASE` along the same path.
            same("SELECT UPPER('stra\u{df}e'), LOWER('STRA\u{df}E'), UCASE('\u{1f0}'), \
                  LCASE('\u{130}')"),
            // The nineteen characters whose fold is wider in UTF-8 than they
            // are. 8.3 folds into a buffer the size of the argument and
            // truncates when one of them overflows it, so over a *column*
            // `UPPER('a\u{23f}b')` there is `A\u{2c7e}` with the `b` gone.
            // This engine declines the fold instead, which is what
            // `general_ci` answers and therefore what 8.3 answers here -- so
            // these are literals, and the column form is the one that
            // diverges. #0154.
            same("SELECT HEX(UPPER('\u{23f}')), HEX(UPPER('a\u{23f}b')), \
                  HEX(UPPER('\u{23f}aa')), HEX(LOWER('\u{23a}'))"),
            same("SELECT HEX(UPPER('\u{23f}\u{23f}a')), HEX(UPPER('aa\u{23f}a')), \
                  LENGTH(UPPER('a\u{23f}b')), CHAR_LENGTH(UPPER('\u{23f}'))"),
            same("SELECT HEX(LOWER('\u{23e}')), HEX(LOWER('\u{130}\u{23a}')), \
                  HEX(UPPER('\u{2c7e}'))"),
            // Through a column, where 8.3 folds by `utf8mb4_0900_ai_ci` --
            // the position the two classes named at the top of this group can
            // only be asked in. Row 6 is a character above the BMP and row 7
            // the `\u{1e9e}` whose lowercase is `\u{df}` under this collation
            // and itself under the other.
            same("SELECT id, HEX(UPPER(t)), HEX(LOWER(t)), LENGTH(UPPER(t)), \
                  CHAR_LENGTH(UPPER(t)) FROM Cf ORDER BY id"),
            same("SELECT id, UPPER(t), LOWER(t) FROM Cf ORDER BY id"),
        ],
    },
    Group {
        // A name is what was written between the quotes, and nothing here may
        // report the *rendering* of it back. Two ways that went wrong, both
        // measured on 8.3 and both silent:
        //
        // `Display` doubles an embedded backtick, so a reader that rendered a
        // name and trimmed the outer pair off reported ``we``ird`` where 8.3
        // reports ``we`ird``; and a reader that split the rendering on `.`
        // could not tell a qualifier from a dot inside a name, so
        // `INSERT INTO \`my.table\`` was 1049 `Unknown database 'my'` against a
        // table `CREATE TABLE` and `SELECT` had both accepted. #0137, #0138.
        //
        // The names 8.3 refuses are refused for the name and not for the
        // quoting — a trailing space is 1103 on a table and 1166 on a column,
        // and so is an empty one — so there is nothing to compare there that
        // the parser does not already settle.
        name: "names as they were written",
        setup: &[
            "CREATE TABLE `we``ird` (`c``ol` INT NOT NULL, `oth er` INT NULL)",
            "CREATE TABLE `oth``er` (`c``ol` INT NULL)",
            "CREATE TABLE `my.table` (a INT NULL)",
            "CREATE TABLE `select` (`from` INT NULL)",
            "CREATE TABLE `tabké` (`colé` INT NULL)",
            // The clause spelled four ways. All four are
            // `utf8mb4_general_ci` on 8.3: the backquoted one was stored with
            // its backticks, and the upper-case one was stored shouting,
            // because 8.3 lowercases the name it records.
            "CREATE TABLE Coll (a VARCHAR(10) COLLATE `utf8mb4_general_ci` NULL, \
             b VARCHAR(10) COLLATE utf8mb4_general_ci NULL, \
             c VARCHAR(10) COLLATE 'utf8mb4_general_ci' NULL, \
             d VARCHAR(10) COLLATE UTF8MB4_GENERAL_CI NULL)",
            // Every type, for the one question this table can answer about
            // them: which of them has a collation at all.
            //
            // Named in lower case on purpose, as `ins` below is. 8.3 declares
            // `information_schema` `TABLE_NAME` `utf8mb3_bin`, so both `ORDER
            // BY` and `=` over it are case-*sensitive* there — `Bee` sorts
            // before `apple` and `= 'bee'` matches nothing. This engine
            // compares every string case-insensitively and has no collation to
            // follow, so a group mixing the cases would diverge on the row
            // order of the listing above and report that gap rather than this
            // one. Lower case throughout is the same answer on both.
            "CREATE TABLE ty (i INT NULL, d DATE NULL, dt DATETIME(3) NULL, f DOUBLE NULL, \
             n DECIMAL(10,2) NULL, t TEXT NULL, v VARCHAR(10) NULL, b BLOB NULL, \
             vb VARBINARY(10) NULL, e ENUM('x','y') NULL, st SET('p','q') NULL, j JSON NULL)",
            "CREATE TABLE ins (a INT NULL, b INT NULL)",
            "CREATE INDEX `ix``x` ON `we``ird` (`c``ol`)",
            "INSERT INTO `we``ird` VALUES (7, 1)",
            "INSERT INTO `oth``er` VALUES (7)",
            "INSERT INTO `my.table` VALUES (1)",
            "INSERT INTO `select` VALUES (3)",
            "INSERT INTO `tabké` VALUES (5)",
            // Setup rather than a case, because every case runs twice — once
            // per protocol — and an index created twice is 1061 on the second.
            "CREATE INDEX ixa ON `my.table` (a)",
            // Here for the same reason, and it is a claim of its own: an
            // assignment target reads its column name by a path nothing below
            // reaches, so if it still read a rendering this fails as 1054 and
            // takes the whole run with it.
            "UPDATE `we``ird` SET `oth er` = 4",
        ],
        cases: &[
            // Where a name is *reported*, which is the only place the
            // difference between a name and its rendering shows.
            same("SELECT TABLE_NAME FROM information_schema.tables \
                  WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME"),
            same("SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME, ORDINAL_POSITION"),
            same("SELECT INDEX_NAME, COLUMN_NAME, TABLE_NAME FROM information_schema.statistics \
                  WHERE TABLE_SCHEMA = DATABASE() ORDER BY INDEX_NAME"),
            same("SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Coll' \
                  ORDER BY ORDINAL_POSITION"),
            // Only a column of characters has one. This reported the
            // database's collation for every column of every type, so an INT
            // claimed `utf8mb4_0900_ai_ci`; nothing asked until this line.
            same("SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'ty' \
                  ORDER BY ORDINAL_POSITION"),
            // And on the wire, where a column's name is what the client reads
            // the value back by.
            same("SELECT `c``ol` AS `al``ias` FROM `we``ird`"),
            same("SELECT `colé` FROM `tabké`"),
            same("SELECT `from` FROM `select`"),

            // The dotted name, statement by statement. `CREATE` and `SELECT`
            // agreed with 8.3 while the three writes beside them did not.
            same("SELECT a FROM `my.table` ORDER BY a"),
            same("INSERT INTO `my.table` (a) VALUES (2)"),
            same("UPDATE `my.table` SET a = 3 WHERE a = 2"),
            same("DELETE FROM `my.table` WHERE a = 3"),
            // Recognised outside the parser, and it took its table name out of
            // the statement's text rather than out of a tree.
            same("ALTER TABLE `my.table` RENAME INDEX ixa TO ixb"),
            same("SELECT INDEX_NAME, TABLE_NAME FROM information_schema.statistics \
                  WHERE TABLE_SCHEMA = DATABASE() ORDER BY INDEX_NAME, TABLE_NAME"),

            // A USING list reads its column name by a path of its own, and
            // the `oth er` beside it is the value the setup's UPDATE wrote.
            same("SELECT o.`c``ol`, w.`oth er` FROM `we``ird` w JOIN `oth``er` o USING (`c``ol`)"),
            same("DROP INDEX `ix``x` ON `we``ird`"),

            // A qualifier on a column in an INSERT's list names the table it
            // is in, and the column is still the last part. Matching on the
            // whole join looked for a column called `Ins.a`.
            same("INSERT INTO ins (ins.a, ins.b) VALUES (1, 2)"),
            same("SELECT a, b FROM ins ORDER BY a"),

            // Deliberately not here: `PREPARE s FROM 'SELECT ''x'' AS c'`,
            // which is the same defect over a literal — trimming the quotes off
            // the ends ate the doubling and prepared the unparseable
            // `SELECT ''x`. 8.3 answers 1295 to a PREPARE sent *through* the
            // prepared-statement protocol, and every case here is asked through
            // both, so the case would report that gap rather than this one. It
            // is covered by a test instead.
        ],
    },
    Group {
        // A collation reaches a client by three routes that have to agree:
        // `information_schema.COLLATIONS` says which exist, `COLLATION_NAME`
        // says which a column has, and a `COLLATE` clause is checked against
        // the first. They were three separate readers, and each pair
        // disagreed. #0138.
        //
        // What this group deliberately cannot ask: `COLLATE utf8mb4_bin`, and
        // every other collation 8.3 has and this engine cannot compare in. 8.3
        // accepts those and this engine refuses them — a refusal rather than a
        // wrong answer, on the ground that recording one and then comparing the
        // column the way every other one is compared answers the wrong
        // question. That is a deliberate divergence, so a case for it would be
        // a baseline line rather than a green run. The refusal is covered by a
        // test.
        //
        // Named in lower case for the reason the group above is: 8.3 declares
        // `information_schema` `TABLE_NAME` `utf8mb3_bin`, so ordering over it
        // is case-sensitive there and is not here.
        name: "collations as MySQL records them",
        setup: &[
            // The table-level clause, which was parsed and dropped. It sets
            // `TABLE_COLLATION` and reaches every character column that
            // declares none of its own.
            "CREATE TABLE tcoll (a VARCHAR(10) NULL, b INT NULL, \
             c TEXT COLLATE utf8mb4_general_ci NULL) \
             DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci",
            // The column-level clause spelled four ways, all one collation.
            "CREATE TABLE ccoll (a VARCHAR(10) COLLATE `utf8mb4_unicode_ci` NULL, \
             b VARCHAR(10) COLLATE utf8mb4_unicode_ci NULL, \
             c VARCHAR(10) COLLATE 'utf8mb4_unicode_ci' NULL, \
             d VARCHAR(10) COLLATE UTF8MB4_UNICODE_CI NULL)",
            // A view reports the collation of the column behind it.
            "CREATE TABLE vbase (a VARCHAR(10) COLLATE utf8mb4_general_ci NULL, n INT NULL)",
            "CREATE VIEW vcoll AS SELECT a, n FROM vbase",
        ],
        cases: &[
            same("SELECT TABLE_NAME, TABLE_COLLATION FROM information_schema.tables \
                  WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME"),
            same("SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'tcoll' \
                  ORDER BY ORDINAL_POSITION"),
            same("SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'ccoll' \
                  ORDER BY ORDINAL_POSITION"),
            same("SELECT COLUMN_NAME, COLLATION_NAME FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'vcoll' \
                  ORDER BY ORDINAL_POSITION"),
            // A name that is not a collation at all. Both servers refuse it,
            // with the same code, which is what makes it askable here where
            // the collations this engine cannot honour are not.
            same("CREATE TABLE bogus1 (a VARCHAR(10) COLLATE bogus_ci)"),
            same("CREATE TABLE bogus2 (a VARCHAR(10)) COLLATE=bogus_ci"),
            same("SELECT 'a' COLLATE bogus_ci"),
        ],
    },
    Group {
        // #0116. SIGN reads anything it is given as a number and answers -1, 0
        // or 1 as a signed LONGLONG — which is not the rule its neighbour ABS
        // follows, and is the half of this worth asking twice: `ABS(1.50)` is
        // a NEWDECIMAL and `ABS(iu)` an *unsigned* LONGLONG, where SIGN of
        // either is a plain signed one. Only the binary protocol notices the
        // difference between a signed and an unsigned 1.
        //
        // The columns are here for the readings that are not the value's own:
        // a temporal is its packed digits, so a negative TIME is -1; an ENUM
        // is its ordinal, so every member is 1; and a string is its leading
        // number, so 'abc' is 0 rather than NULL.
        name: "sign",
        setup: &[
            "CREATE TABLE Sg (id INT NOT NULL, i INT NULL, iu BIGINT UNSIGNED NULL, \
             b BIGINT NULL, d2 DECIMAL(10,2) NULL, dw DECIMAL(65,30) NULL, \
             dz DECIMAL(6,2) NULL, f FLOAT NULL, g DOUBLE NULL, s VARCHAR(20) NULL, \
             dt DATE NULL, tm TIME NULL, e ENUM('a','b','c') NULL, nn INT NOT NULL, \
             PRIMARY KEY (id))",
            // Row 1 is negative wherever a column can be, row 2 positive, row
            // 3 zero, and row 4 NULL. The zero row is the one that matters
            // most: it is the only place `v > 0` and `v >= 0` differ, and the
            // only place a signed zero can leak a -1.
            "INSERT INTO Sg VALUES (1, -5, 0, -9223372036854775808, -1.50, \
             -0.000000000000000000000000000001, -0.001, -1.5, -2.5, '-12abc', \
             '2020-01-02', '-01:00:00', 'a', -7)",
            "INSERT INTO Sg VALUES (2, 5, 18446744073709551615, 9223372036854775807, \
             1.50, 0.000000000000000000000000000001, 0.001, 1.5, 2.5, '12abc', \
             '2026-01-02', '01:00:00', 'c', 7)",
            "INSERT INTO Sg VALUES (3, 0, 0, 0, 0.00, 0.000000000000000000000000000000, \
             0.00, 0, 0, 'abc', '2026-01-02', '00:00:00', 'b', 0)",
            "INSERT INTO Sg (id, nn) VALUES (4, 0)",
            // Written to by the cases below, which is why it is empty here: an
            // INSERT's values are the one clause with no row to evaluate
            // against, so a refusal that arrives per row arrives after the
            // write.
            "CREATE TABLE Ins (i INT NULL, u INT NOT NULL, PRIMARY KEY (u))",
        ],
        cases: &[
            same("SELECT SIGN(5), SIGN(-5), SIGN(0)"),
            // The ends of the signed range and the first value past it, which
            // is where the reading changes from an i64 to a u64.
            same("SELECT SIGN(9223372036854775807), SIGN(-9223372036854775808), \
                  SIGN(18446744073709551615)"),
            same("SELECT SIGN(1.50), SIGN(-1.50), SIGN(0.00), SIGN(-0.0)"),
            same("SELECT SIGN(1.5e0), SIGN(-1.5e0), SIGN(0e0), SIGN(-0.0e0)"),
            same("SELECT SIGN('12'), SIGN('-12'), SIGN('abc'), SIGN(NULL)"),
            same("SELECT SIGN(TRUE), SIGN(FALSE), SIGN(1/0)"),
            // The pair that says the decimal is not read through a double.
            // MySQL answers 0 for the exact sum and 1 for the approximate one,
            // because the double is 5.55e-17 and the decimal is zero.
            same("SELECT SIGN(0.1+0.2-0.3), SIGN(0.1e0+0.2e0-0.3e0)"),
            // A value that is zero only after it is rounded is zero, and has
            // no sign left to report in either direction.
            same("SELECT SIGN(CAST(0.001 AS DECIMAL(10,2))), \
                  SIGN(CAST(-0.001 AS DECIMAL(10,2)))"),
            same("SELECT id, SIGN(i), SIGN(iu), SIGN(b), SIGN(nn) FROM Sg ORDER BY id"),
            same("SELECT id, SIGN(d2), SIGN(dw), SIGN(dz), SIGN(f), SIGN(g) \
                  FROM Sg ORDER BY id"),
            same("SELECT id, SIGN(s), SIGN(dt), SIGN(tm), SIGN(e) FROM Sg ORDER BY id"),
            // ABS beside it, because "signed LONGLONG" only means something
            // against a function that answers otherwise for the same argument.
            same("SELECT id, ABS(iu), SIGN(iu), ABS(d2), SIGN(d2) FROM Sg ORDER BY id"),
            same("SELECT id, SIGN(ABS(i)), ABS(SIGN(i)) FROM Sg ORDER BY id"),
            // The shapes an application writes: a sign in a predicate, and a
            // sign over an aggregate.
            same("SELECT id FROM Sg WHERE SIGN(i) = -1 ORDER BY id"),
            same("SELECT SIGN(SUM(i)), SIGN(MIN(d2)), SIGN(MAX(g)) FROM Sg"),
            same("SELECT SIGN(i) AS s, COUNT(*) FROM Sg GROUP BY SIGN(i) ORDER BY s"),
            // No rows, so only the column definition answers — and that is the
            // half of SIGN a populated table cannot check on its own.
            same("SELECT SIGN(i), SIGN(iu), SIGN(d2) FROM Sg WHERE 1=0"),
            // 1582, which 8.3 raises from the parser and therefore raises over
            // an empty relation and down a branch nothing takes. A refusal
            // settled per row would answer all four of these.
            same("SELECT SIGN()"),
            same("SELECT SIGN(1, 2)"),
            same("SELECT SIGN(1, 2, 3)"),
            same("SELECT SIGN(i, i) FROM Sg WHERE 1=0"),
            same("SELECT CASE WHEN 0 THEN SIGN(1, 2) ELSE 9 END"),
            // Against the other refusal settled at the same point, which it
            // has to beat: 8.3 answers 1582 here and not 1210.
            same("SELECT ('a' LIKE 'b' ESCAPE '!!') AND SIGN() > 0"),
            // The write half, in order: three refusals, one that goes through,
            // and then the table — which holds exactly the one row on both
            // servers, so the refusals wrote nothing.
            //
            // Every case runs twice, once per protocol, so the one that goes
            // through is written to be idempotent: without the ON DUPLICATE
            // clause its second run would be a 1062 and the case would be
            // asking about a collision instead.
            same("INSERT INTO Ins VALUES (SIGN(1, 2), 1)"),
            same("INSERT INTO Ins VALUES (1, 2), (SIGN(1, 2), 3)"),
            same("INSERT INTO Ins VALUES (1, 4) ON DUPLICATE KEY UPDATE i = SIGN(1, 2)"),
            same("INSERT INTO Ins VALUES (SIGN(-2), 5) ON DUPLICATE KEY UPDATE i = SIGN(-2)"),
            same("SELECT i, u FROM Ins ORDER BY u"),
            // A declaration's expressions are stored and evaluated per row, so
            // a CREATE accepted here would leave a table 8.3 could not make.
            // A refused CREATE makes nothing, so these are the same question
            // on both of a case's protocol passes.
            same("CREATE TABLE Gen (a INT NULL, b INT AS (SIGN(1, 2)))"),
            same("CREATE TABLE Def (a INT NULL DEFAULT (SIGN(1, 2)))"),
            // The expressions a call carries that are not its arguments.
            same("SELECT GROUP_CONCAT(id ORDER BY SIGN(1, 2)) FROM Sg"),
            same("SELECT SUM(id) OVER (ORDER BY SIGN(1, 2)) FROM Sg"),
        ],
    },
    Group {
        // The id an insert hands back. MySQL reports the *first* id a
        // statement generated, and this engine reported the last -- while its
        // own `CREATE TABLE ... AS SELECT` path, written later, reported the
        // first. Two paths disagreeing with each other. #0132.
        //
        // Every case here is ordered against the ones before it, because
        // `LAST_INSERT_ID()` is per-connection state and the two connections
        // outlive every group. The group is made immune to what ran before it
        // by ending its setup with an insert, so the first case reads a value
        // this group put there; and nothing outside it reads the value, which
        // is what makes leaving one behind harmless. Any group added later that
        // asks the same question needs the same treatment.
        //
        // What this group deliberately cannot ask: the *next* id after a
        // statement whose row count MySQL does not know in advance. InnoDB
        // reserves a batch for those and discards what it did not use, so the
        // id after a two-row `INSERT ... SELECT` is 4 on 8.3 and 3 here. That
        // is the storage engine's allocator rather than SQL, so no case reads a
        // generated id out of `LiSel` after its setup. `Li` is the one table
        // whose ids are asked about, and every statement that touches it is a
        // VALUES list of a length both servers know before they start.
        name: "the id an insert generated",
        setup: &[
            "CREATE TABLE Li (id INT NOT NULL AUTO_INCREMENT, a INT NULL, PRIMARY KEY (id))",
            "CREATE TABLE LiUp (id INT NOT NULL AUTO_INCREMENT, k INT NOT NULL, v INT NULL, \
             PRIMARY KEY (id), UNIQUE KEY uk_liup_k (k))",
            "INSERT INTO LiUp (k, v) VALUES (1, 10)",
            "CREATE TABLE LiOwn (id INT NOT NULL AUTO_INCREMENT, a INT NULL, PRIMARY KEY (id))",
            "CREATE TABLE LiSrc (a INT NULL)",
            "INSERT INTO LiSrc VALUES (1), (2)",
            "CREATE TABLE LiNone (a INT NULL)",
            "CREATE TABLE LiSel (id INT NOT NULL AUTO_INCREMENT, a INT NULL, PRIMARY KEY (id))",
            // Last, and the value the first case reads: two rows in, and the
            // answer is the first of the two ids rather than the second.
            "INSERT INTO LiSel (a) SELECT a FROM LiSrc",
        ],
        cases: &[
            same("SELECT LAST_INSERT_ID() AS from_a_two_row_insert_select"),
            // Run once per protocol, so this fills `Li` twice: ids 1 to 3 and
            // then 4 to 6, and the case below reads 4. A multi-row VALUES list
            // is a row count MySQL knows in advance, so neither server leaves
            // a gap and the second batch starts where the first ended.
            same("INSERT INTO Li (a) VALUES (10), (20), (30)"),
            same("SELECT LAST_INSERT_ID() AS from_a_three_row_values_insert"),
            // An upsert that only ever updates -- `v` moves on both runs, so
            // both report two rows affected. It generates no id, and must not
            // erase the one before it.
            same("INSERT INTO LiUp (k, v) VALUES (1, 99) ON DUPLICATE KEY UPDATE v = v + 1"),
            same("SELECT LAST_INSERT_ID() AS after_an_upsert_that_only_updated"),
            // A row that supplies its own id generates none. IGNORE rather
            // than a bare insert because the second protocol runs it again and
            // would otherwise collide; skipping the row is the same answer on
            // both servers, and still no id.
            same("INSERT IGNORE INTO LiOwn (id, a) VALUES (100, 1)"),
            same("SELECT LAST_INSERT_ID() AS after_an_insert_that_supplied_its_own_id"),
            // No rows at all, which generates nothing and moves no counter.
            same("INSERT INTO LiSel (a) SELECT a FROM LiNone"),
            same("SELECT LAST_INSERT_ID() AS after_an_insert_of_no_rows"),
            // Back to `Li`, where the counter has not been disturbed by any of
            // the above: the two runs take 7 and 8, and the answer is 8.
            same("INSERT INTO Li (a) VALUES (40)"),
            same("SELECT LAST_INSERT_ID() AS from_a_single_row_insert"),
        ],
    },
    Group {
        // A SELECT with no FROM, clause by clause. #0158: the path evaluated
        // its projection and its HAVING and dropped the WHERE and the GROUP BY
        // on the floor, so `SELECT 1 WHERE 0` answered a row — a wrong answer
        // in one of the simplest statements SQL has, and one that hid every
        // refusal written inside the clause along with it.
        //
        // The point of a group of its own is that this shape has no rows to
        // hide behind: a corpus case over a populated table cannot reach any
        // of it.
        name: "a select with no FROM",
        setup: &[
            "CREATE TABLE Fl (id INT NOT NULL, b INT NULL, PRIMARY KEY (id))",
            "INSERT INTO Fl VALUES (1, 7), (2, 8)",
        ],
        cases: &[
            // Every spelling of a falsehood, because the clause was not
            // evaluated at all rather than evaluated wrongly.
            same("SELECT 1 WHERE 0"),
            same("SELECT 1 WHERE 1 = 0"),
            same("SELECT 1 WHERE NULL"),
            same("SELECT 1 WHERE 'a'"),
            same("SELECT 1 WHERE 0.0"),
            same("SELECT 1 WHERE CAST('x' AS SIGNED)"),
            // And the truths beside them, so a filter that rejects everything
            // is not mistaken for one that works.
            same("SELECT 1 WHERE 1"),
            same("SELECT 1 WHERE -1"),
            same("SELECT 1 WHERE '1abc'"),
            // A predicate that has to run a query of its own to settle.
            same("SELECT 1 WHERE (SELECT 0)"),
            same("SELECT 1 WHERE 1 IN (SELECT 2)"),
            same("SELECT 1 WHERE 1 IN (SELECT 1)"),
            same("SELECT 1 WHERE EXISTS (SELECT 1 WHERE 0)"),
            // The names, under the clause each belongs to. A select-list alias
            // is not in scope in a WHERE, and the field list is settled first.
            same("SELECT 1 WHERE zz = 1"),
            same("SELECT 1 AS a WHERE a = 1"),
            same("SELECT nope WHERE zz = 1"),
            // The refusals #0158 recorded as masked. The last two are written
            // where nothing evaluates them, which is what separates a refusal
            // settled while the item is resolved from one the evaluator
            // happens to reach.
            same("SELECT 1 WHERE 'x' LIKE 'a' ESCAPE '!!'"),
            same("SELECT 1 WHERE 1 IN (SELECT SIGN(1, 2))"),
            same("SELECT 1 WHERE CASE WHEN 0 THEN ('a' LIKE 'b' ESCAPE '!!') ELSE 9 END"),
            same("SELECT 1 WHERE 0 AND SIGN(1, 2)"),
            // An ungrouped aggregate answers one row whatever the WHERE said,
            // and over a rejected implicit row that row is the fold over
            // nothing — which is a different answer from no row at all.
            same("SELECT COUNT(*), SUM(1), MAX(2), MIN(2), AVG(3), GROUP_CONCAT(4) WHERE 0"),
            same("SELECT 1, COUNT(*) WHERE 0"),
            same("SELECT COUNT(DISTINCT 1) WHERE 0"),
            same("SELECT COUNT(*), SUM(1) WHERE 1"),
            // The argument of an aggregate over an empty group is not
            // evaluated, so a runtime error in it is not raised — the pair
            // says so: NULL under `WHERE 0` and 1690 under `WHERE 1`. What it
            // is still held to is what 8.3 settles while resolving.
            same("SELECT SUM(9223372036854775807 + 1) WHERE 0"),
            same("SELECT SUM(9223372036854775807 + 1) WHERE 1"),
            same("SELECT SUM('a' LIKE 'b' ESCAPE '!!') WHERE 0"),
            // An aggregate written only in the HAVING aggregates the query
            // too, so it answers a row where the same statement without one
            // answers none.
            same("SELECT 1 WHERE 0 HAVING COUNT(*) = 0"),
            same("SELECT COUNT(*) WHERE 0 HAVING COUNT(*) = 0"),
            same("SELECT COUNT(*) WHERE 0 HAVING COUNT(*) = 5"),
            // A GROUP BY takes that row away again: no rows means no groups.
            same("SELECT COUNT(*) WHERE 0 GROUP BY 'a'"),
            same("SELECT 1 WHERE 0 GROUP BY 'a'"),
            same("SELECT COUNT(*) WHERE 1 GROUP BY 'a'"),
            // A FROM-less GROUP BY can never change which rows come back, so
            // the whole of what it decides is what it refuses.
            same("SELECT 1 GROUP BY 2"),
            same("SELECT 1 GROUP BY zz"),
            same("SELECT 1 GROUP BY 'x' LIKE 'a' ESCAPE '!!'"),
            same("SELECT 1 AS a GROUP BY a"),
            same("SELECT 1 GROUP BY 1"),
            // Which clause is named when more than one of them is wrong: the
            // WHERE before the GROUP BY, the GROUP BY before the HAVING, and
            // the HAVING even where the WHERE has already emptied the result.
            same("SELECT 1 WHERE zz = 1 GROUP BY 2"),
            same("SELECT 1 GROUP BY 2 HAVING zz > 1"),
            same("SELECT 1 WHERE 0 HAVING zz > 1"),
            // The clauses that were already reached, kept so the fix cannot
            // take one of them away.
            same("SELECT 1 WHERE 0 ORDER BY 1"),
            same("SELECT 1 AS a WHERE 0 ORDER BY 2"),
            same("SELECT 1 WHERE 0 LIMIT 1"),
            same("SELECT 1 WHERE 1 LIMIT 0"),
            same("SELECT DISTINCT 1 WHERE 0"),
            // Every context a FROM-less SELECT can be written in, because the
            // clause has to hold in all of them and not only in a statement of
            // its own.
            same("SELECT * FROM (SELECT COUNT(*) AS c WHERE 0) t"),
            // Counted rather than projected, because a derived table
            // materialises a bare `1` as a LONG on 8.3 and as a LONGLONG here
            // — a divergence of its own, which asking for the column back
            // would report in place of this case's question.
            same("SELECT COUNT(*) FROM (SELECT 1 AS a WHERE 0) t"),
            same("WITH c AS (SELECT 1 AS a WHERE 0) SELECT COUNT(*) FROM c"),
            same("SELECT EXISTS (SELECT 1 WHERE 0)"),
            same("SELECT 1 AS a WHERE 0 UNION ALL SELECT 2"),
            same("SELECT 2 AS a UNION ALL SELECT 1 WHERE 0"),
            // Correlated, so the predicate has to be evaluated against the
            // enclosing row rather than against nothing. Read from the WHERE
            // rather than projected, because a scalar subquery in a select
            // list is declared VAR_STRING by this engine and LONGLONG by 8.3 —
            // a divergence of its own that would drown this case's question.
            same("SELECT id FROM Fl WHERE 1 IN (SELECT 1 WHERE Fl.b > 7) ORDER BY id"),
            same("SELECT id FROM Fl WHERE EXISTS (SELECT 1 WHERE Fl.b > 7) ORDER BY id"),
        ],
    },
    Group {
        name: "casting to an integer",
        setup: &[
            // One column per way a number can arrive, all holding the same
            // half: the point of the group is that they do *not* all cast to
            // the same integer.
            "CREATE TABLE Ci (id INT NOT NULL, d1 DECIMAL(10,1) NULL, d2 DECIMAL(10,2) NULL, \
             d5 DECIMAL(20,5) NULL, wide DECIMAL(20,1) NULL, g DOUBLE NULL, f FLOAT NULL, \
             s VARCHAR(40) NULL, e ENUM('a','b','c') NULL, at DATETIME(3) NULL, \
             tm TIME(3) NULL, bu BIGINT UNSIGNED NULL, PRIMARY KEY (id))",
            // `wide` is 2^52 and a half, which no double holds: a reading that
            // goes through one loses the tie and answers one less.
            "INSERT INTO Ci VALUES (1, 2.5, 2.505, 2.500005, 4503599627370496.5, 2.5, 2.5, \
             '2.5', 'b', '2024-01-05 06:07:08.500', '06:07:08.500', 18446744073709551615)",
            "INSERT INTO Ci VALUES (2, -2.5, -2.505, -2.500005, -4503599627370496.5, -2.5, \
             -2.5, '-2.5', 'c', '2024-01-05 06:07:09.500', '06:07:09.500', 1)",
            "INSERT INTO Ci VALUES (3, 1.5, 1.50, 1.50000, 1.5, 1.5, 1.5, '1.9', 'a', \
             '2024-01-05 00:00:00.000', '00:00:00.000', 0)",
            "INSERT INTO Ci (id) VALUES (4)",
            // The ranges, one row per direction and one inside.
            "CREATE TABLE Rng (id INT NOT NULL, g DOUBLE NULL, d DECIMAL(30,1) NULL, \
             s VARCHAR(40) NULL, PRIMARY KEY (id))",
            "INSERT INTO Rng VALUES (1, 1e30, 99999999999999999999.5, '99999999999999999999')",
            "INSERT INTO Rng VALUES (2, -1e30, -99999999999999999999.5, \
             '-99999999999999999999')",
            "INSERT INTO Rng VALUES (3, 1.5e0, 18446744073709551615.5, '18446744073709551616')",
        ],
        cases: &[
            // An exact operand rounds half away from zero. #0120: this
            // truncated, so the first two were 0 and 1.
            same("SELECT CAST(0.5 AS SIGNED), CAST(1.5 AS SIGNED), CAST(2.5 AS SIGNED), \
                  CAST(3.5 AS SIGNED)"),
            same("SELECT CAST(-0.5 AS SIGNED), CAST(-1.5 AS SIGNED), CAST(-2.5 AS SIGNED), \
                  CAST(-3.5 AS SIGNED)"),
            // Either side of a half, so the group says rounding and not "a
            // fraction adds one".
            same("SELECT CAST(1.49 AS SIGNED), CAST(1.51 AS SIGNED), CAST(-1.49 AS SIGNED), \
                  CAST(-1.51 AS SIGNED)"),
            // An approximate one rounds half to *even*, which is a different
            // rule and not a different bug.
            same("SELECT CAST(0.5e0 AS SIGNED), CAST(1.5e0 AS SIGNED), \
                  CAST(2.5e0 AS SIGNED), CAST(3.5e0 AS SIGNED), CAST(4.5e0 AS SIGNED)"),
            same("SELECT CAST(-0.5e0 AS SIGNED), CAST(-1.5e0 AS SIGNED), \
                  CAST(-2.5e0 AS SIGNED)"),
            // The three rules over one number, which is the case this group
            // exists for: 3, 2 and 2.
            same("SELECT CAST(2.5 AS SIGNED), CAST(2.5e0 AS SIGNED), CAST('2.5' AS SIGNED)"),
            // A string truncates rather than rounding at all.
            same("SELECT CAST('1.5' AS SIGNED), CAST('1.9' AS SIGNED), \
                  CAST('-1.9' AS SIGNED), CAST('0.9' AS SIGNED)"),
            same("SELECT CAST('12abc' AS SIGNED), CAST('abc' AS SIGNED), \
                  CAST('  2.7  ' AS SIGNED), CAST('.5' AS SIGNED)"),
            // It stops at the first character an integer cannot contain, and
            // an exponent is one: read as a *number* instead these were 10^30
            // and an infinity.
            same("SELECT CAST('1e30' AS SIGNED), CAST('1e3' AS SIGNED), \
                  CAST('1.9e3' AS SIGNED), CAST('0x1F' AS SIGNED), \
                  CAST('12 34' AS SIGNED)"),
            same("SELECT CAST('inf' AS SIGNED), CAST('-inf' AS SIGNED), \
                  CAST('nan' AS SIGNED), CAST('inf' AS UNSIGNED)"),
            // A lone sign is 0 and not a saturation.
            same("SELECT CAST('- 12' AS SIGNED), CAST('--12' AS SIGNED), \
                  CAST('' AS SIGNED), CAST('  +12' AS SIGNED)"),
            // More digits than fit, saturating at the unsigned boundary in
            // each direction before the target reinterprets it.
            same("SELECT CAST('99999999999999999999999999999999999999999' AS SIGNED), \
                  CAST('99999999999999999999999999999999999999999' AS UNSIGNED), \
                  CAST('-99999999999999999999999999999999999999999' AS SIGNED), \
                  CAST('-99999999999999999999999999999999999999999' AS UNSIGNED)"),
            same("SELECT CAST(NULL AS SIGNED), CAST(NULL AS UNSIGNED)"),
            // UNSIGNED shares the reading: a negative rounds first and wraps
            // second, which is what puts -1.5 two below the maximum.
            same("SELECT CAST(1.5 AS UNSIGNED), CAST(2.5 AS UNSIGNED), \
                  CAST(-1.5 AS UNSIGNED), CAST(-2.5 AS UNSIGNED)"),
            same("SELECT CAST(-0.5 AS UNSIGNED), CAST(-0.4 AS UNSIGNED), \
                  CAST(-1 AS UNSIGNED)"),
            same("SELECT CAST(1.5e0 AS UNSIGNED), CAST(-1.5e0 AS UNSIGNED), \
                  CAST(-2.5e0 AS UNSIGNED)"),
            // The longer spellings of the same two targets.
            same("SELECT CAST(1.5 AS SIGNED INTEGER), CAST(1.5 AS UNSIGNED INTEGER)"),
            // A decimal that arrived by arithmetic or by a cast, which is
            // where an application meets this.
            same("SELECT CAST(1.25 + 1.25 AS SIGNED), CAST(5/2 AS SIGNED), \
                  CAST(CAST('2.5' AS DECIMAL(10,1)) AS SIGNED)"),
            // An exact operand saturates into the target's own range.
            same("SELECT CAST(99999999999999999999 AS SIGNED), \
                  CAST(-99999999999999999999 AS SIGNED), \
                  CAST(99999999999999999999 AS UNSIGNED), \
                  CAST(-99999999999999999999 AS UNSIGNED)"),
            // Past the 38 digits an i128 counts, where only the sign is left
            // to answer with. MySQL's DECIMAL holds 65, so these are numbers
            // it converts rather than refuses.
            same("SELECT CAST(99999999999999999999999999999999999999999 AS SIGNED), \
                  CAST(-99999999999999999999999999999999999999999 AS SIGNED), \
                  CAST(99999999999999999999999999999999999999999 AS UNSIGNED), \
                  CAST(-99999999999999999999999999999999999999999 AS UNSIGNED)"),
            // An approximate operand converts through a *signed* longlong
            // whichever integer it was asked for, so 1e19 stops at the signed
            // maximum where the same size as a decimal reaches the unsigned
            // one.
            same("SELECT CAST(1e19 AS UNSIGNED), CAST(1e30 AS SIGNED), \
                  CAST(1e30 AS UNSIGNED)"),
            // A string saturates at the unsigned boundary and the signed
            // target reinterprets those bits: -1 for digits a decimal literal
            // answers 9223372036854775807 for.
            same("SELECT CAST('99999999999999999999' AS SIGNED), \
                  CAST('99999999999999999999' AS UNSIGNED)"),
            // An integer literal above i64::MAX and inside u64 is one MySQL
            // holds unsigned, so the signed cast hands its bits back.
            same("SELECT CAST(10000000000000000000 AS SIGNED), \
                  CAST(10000000000000000000 AS UNSIGNED)"),
            // A half at the very edge of each range, where rounding and
            // saturation meet.
            same("SELECT CAST(9223372036854775807.5 AS SIGNED), \
                  CAST(18446744073709551615.5 AS UNSIGNED)"),
            // Stored rather than written, because a column is where the scale
            // is a declaration and not a spelling.
            same("SELECT id, CAST(d1 AS SIGNED), CAST(d2 AS SIGNED), CAST(d5 AS SIGNED), \
                  CAST(wide AS SIGNED) FROM Ci ORDER BY id"),
            same("SELECT id, CAST(g AS SIGNED), CAST(f AS SIGNED), CAST(s AS SIGNED) \
                  FROM Ci ORDER BY id"),
            // The readings that are neither: an ENUM converts through its
            // ordinal and a temporal through its packed number, which rounds
            // half away from zero like the decimal it is.
            same("SELECT id, CAST(e AS SIGNED), CAST(at AS SIGNED), CAST(tm AS SIGNED), \
                  CAST(bu AS SIGNED) FROM Ci ORDER BY id"),
            same("SELECT id, CAST(d1 AS UNSIGNED), CAST(g AS UNSIGNED), \
                  CAST(f AS UNSIGNED), CAST(bu AS UNSIGNED) FROM Ci ORDER BY id"),
            same("SELECT id, CAST(g AS SIGNED), CAST(g AS UNSIGNED) FROM Rng ORDER BY id"),
            same("SELECT id, CAST(d AS SIGNED), CAST(d AS UNSIGNED) FROM Rng ORDER BY id"),
            same("SELECT id, CAST(s AS SIGNED), CAST(s AS UNSIGNED) FROM Rng ORDER BY id"),
            // Where the rounding decides a row rather than a printed value.
            same("SELECT id FROM Ci WHERE CAST(d1 AS SIGNED) = 3 ORDER BY id"),
            same("SELECT id FROM Ci ORDER BY CAST(d1 AS SIGNED), id"),
        ],
    },
    Group {
        name: "a scalar subquery's column",
        setup: &[
            "CREATE TABLE Sq (id INT NOT NULL AUTO_INCREMENT, n INT NOT NULL, \
             un INT UNSIGNED NOT NULL, sm SMALLINT NULL, s VARCHAR(32) NOT NULL, \
             c CHAR(4) NULL, dc DECIMAL(10,3) NOT NULL, f FLOAT NULL, g DOUBLE NULL, \
             dd DATE NULL, e ENUM('a','b','c') NOT NULL, bl BLOB NULL, bn BINARY(4) NULL, \
             tx TEXT NULL, PRIMARY KEY (id))",
            "CREATE TABLE SqE (id INT NOT NULL, v INT NOT NULL, PRIMARY KEY (id))",
            "INSERT INTO Sq (n, un, sm, s, c, dc, f, g, dd, e, bl, bn, tx) \
             VALUES (1, 7, 3, 'hello', 'ab', 12.500, 1.5, 2.5, '2026-01-02', 'b', \
             'bytes', 'binx', 'text')",
        ],
        cases: &[
            // The two #0104 was filed with. Every column in this group was a
            // VAR_STRING before it, so the binary protocol sent the digits of
            // a number as text and a prepared-statement client read a string.
            same("SELECT (SELECT COUNT(*) FROM Sq)"),
            same("SELECT (SELECT MAX(t.n) FROM Sq t)"),
            // A plain column of each family, because the answer is the *inner*
            // column's type and not a type of the subquery's own: an `INT` is
            // a LONG and a `SMALLINT` a SHORT, and the sign travels with them.
            same("SELECT (SELECT n FROM Sq), (SELECT sm FROM Sq), (SELECT un FROM Sq)"),
            same("SELECT (SELECT dc FROM Sq), (SELECT f FROM Sq), (SELECT g FROM Sq)"),
            same("SELECT (SELECT s FROM Sq), (SELECT c FROM Sq), (SELECT tx FROM Sq)"),
            // Bytes rather than characters, which is the one part of a
            // computed column's definition this harness compares: a `BLOB` and
            // a `BINARY` come through binary where the `TEXT` beside them does
            // not.
            same("SELECT (SELECT bl FROM Sq), (SELECT bn FROM Sq)"),
            // An ENUM arrives as its member under a STRING code and loses the
            // ENUM flag on the way through, on both servers.
            same("SELECT (SELECT e FROM Sq)"),
            // The width and the flags are out of scope for a computed column,
            // so this asks the type and the value. Not the character set: 8.3
            // reports a temporal that has been through a scalar subquery in
            // the connection's, where the column read straight from its table
            // is binary — the same quirk `MIN(tm)` above declines, and this
            // engine has no temporary table to model it with.
            same("SELECT (SELECT dd FROM Sq)").without_value_kind(),
            // A literal is typed by what it is, and keeps NOT_NULL: a
            // subquery over no table answers whatever the database holds.
            same("SELECT (SELECT 1), (SELECT 1.5), (SELECT 1e3), (SELECT 'x')"),
            // #0073's pair, from the other side. A bare `SELECT NULL` has no
            // type; through a set operation or a derived table it is the
            // `binary(0)` a temporary table gives it, and a scalar subquery
            // reports whichever of those the inner query answers with.
            same("SELECT (SELECT NULL)"),
            same("SELECT (SELECT NULL UNION SELECT NULL)"),
            same("SELECT (SELECT c2 FROM (SELECT NULL AS c2) d)"),
            // Nothing about the rows decides the column: over an empty table,
            // under a LIMIT, behind a false predicate and through a set
            // operation it is the same type carrying a different value.
            same("SELECT (SELECT v FROM SqE)"),
            same("SELECT (SELECT n FROM Sq LIMIT 1)"),
            same("SELECT (SELECT n FROM Sq WHERE 1 = 0), (SELECT n FROM Sq LIMIT 0)"),
            same("SELECT (SELECT n FROM Sq UNION SELECT n FROM Sq)"),
            // More than one row is 1242 on both, and at execution rather than
            // at prepare — 8.3 describes this statement and then refuses it.
            same("SELECT (SELECT n FROM Sq UNION ALL SELECT n FROM Sq)"),
            // Brackets peel and nesting recurses.
            same("SELECT ((SELECT n FROM Sq)), (SELECT (SELECT (SELECT n FROM Sq)))"),
            // The expression around it reads the column too, which is where a
            // subquery with no type of its own was making everything
            // approximate: arithmetic over an operand nothing can type is a
            // DOUBLE.
            same("SELECT (SELECT n FROM Sq) + 1, (SELECT un FROM Sq) + 1"),
            same(
                "SELECT ABS((SELECT n FROM Sq)), ROUND((SELECT dc FROM Sq), 1), \
                 NULLIF((SELECT sm FROM Sq), 1)",
            ),
            same("SELECT CASE WHEN 1 THEN (SELECT sm FROM Sq) ELSE 1000 END"),
            same("SELECT COALESCE((SELECT s FROM Sq), 1), CONCAT((SELECT n FROM Sq), 'x')"),
            same("SELECT (SELECT e FROM Sq) + 0, (SELECT e FROM Sq) = 'b'"),
            same("SELECT MIN((SELECT n FROM Sq)) FROM Sq"),
            same("SELECT (SELECT SUM(t.n) FROM Sq t), (SELECT AVG(t.n) FROM Sq t)"),
            // The NOT_NULL flag needs the query and not only the column. A
            // WHERE and a HAVING can leave the one implicit row unmatched, so
            // both of these are nullable; an ORDER BY cannot.
            same("SELECT (SELECT 1 WHERE 0), (SELECT COUNT(*)), (SELECT 1 ORDER BY 1), \
                  (SELECT 1 HAVING 1)"),
            // The set operators split on the same question: a UNION of two
            // certain branches answers a row, and the two that take rows away
            // do not — both of those are empty, so both are NULL and both are
            // nullable on 8.3, where reading the operation and not the
            // operator would have called them NOT NULL.
            same("SELECT (SELECT 1 UNION SELECT 1), (SELECT 1 EXCEPT SELECT 1), \
                  (SELECT 1 INTERSECT SELECT 2)"),
            // The three projections that type a column, because they are three
            // pieces of code: a FROM-less select, a select over a table, and a
            // grouped one.
            same("SELECT (SELECT 1) FROM Sq"),
            same("SELECT id, (SELECT t.s FROM Sq t WHERE t.id = Sq.id) AS lab \
                  FROM Sq ORDER BY id"),
            same("SELECT n, (SELECT COUNT(*) FROM SqE) AS c FROM Sq GROUP BY n ORDER BY n"),
            // Correlated, and projected rather than compared: the enclosing
            // row has to be in scope while the column is described as well as
            // while it is evaluated.
            same("SELECT (SELECT COUNT(*) FROM SqE WHERE SqE.v = Sq.id) AS c \
                  FROM Sq ORDER BY c"),
            // The clauses where the type is not reported at all, so that
            // describing the subquery cannot have broken resolving it.
            same("SELECT id FROM Sq WHERE id = (SELECT MAX(t.id) FROM Sq t) ORDER BY id"),
            same("SELECT id FROM Sq ORDER BY (SELECT MAX(t.id) FROM Sq t), id"),
        ],
    },
    Group {
        // MySQL takes an identifier of at most 64 *characters* — the same
        // number whatever the bytes cost, so a 64-character name may be 256
        // bytes long — and refuses a longer one outright. Accepting a table
        // this engine can create and MySQL cannot is the failure that matters
        // here: a migration passes against the test database and fails against
        // the real server. #0107.
        //
        // An alias is not that limit and is not refused at all: it is cut to
        // 256 bytes, on a character boundary, and the cut name is the one a
        // later clause resolves against. Three limits on three paths, the
        // third being the 255-byte cut on a generated name that #0059
        // implemented.
        name: "how long an identifier may be",
        setup: &[
            "CREATE TABLE Ident (i INT NOT NULL, PRIMARY KEY (i))",
            "INSERT INTO Ident VALUES (1)",
        ],
        cases: &[
            // A column, a table, a database, an index, a key and a
            // constraint: every kind that names something is 1059 at 65.
            same("CREATE TABLE Idl1 (`aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` INT)"),
            same("CREATE TABLE `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` (i INT)"),
            same("CREATE DATABASE `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            same("CREATE INDEX `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` ON Ident (i)"),
            same("CREATE TABLE Idl2 (i INT, KEY `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` (i))"),
            same("CREATE TABLE Idl3 (i INT, CONSTRAINT `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` CHECK (i > 0))"),
            same("ALTER TABLE Ident ADD COLUMN `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` INT"),
            same("CREATE VIEW `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` AS SELECT 1"),
            // The paths MySQL answers 1166 for instead, which is the same
            // limit reached where a column name is derived rather than
            // defined. The two codes are not interchangeable.
            same("ALTER TABLE Ident RENAME COLUMN i TO `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            same("CREATE TABLE Idl4 AS SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            same("CREATE VIEW Idv1 AS SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            same("CREATE VIEW Idv2 (`aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`) AS SELECT 1"),
            // A branch of a union and a derived table name no column of the
            // view, and 8.3 refuses them all the same.
            same("CREATE VIEW Idv3 AS SELECT 1 AS x UNION SELECT 2 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            same("CREATE VIEW Idv4 AS SELECT * FROM (SELECT 'x' AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`) d"),
            // Settled while the statement is read, so it beats the refusals a
            // later stage would raise: a table that is not there, a table that
            // already is, and a second column of one name.
            same("ALTER TABLE Nope ADD COLUMN `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` INT"),
            same("CREATE TABLE Ident (`aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` INT)"),
            same("CREATE TABLE IF NOT EXISTS Ident (`aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` INT)"),
            same("CREATE TABLE Idl5 (b INT, b INT, `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` INT)"),
            // The measured exception: CREATE INDEX finds its table first, so
            // this one is 1146 where the ALTER above is 1059.
            same("CREATE INDEX `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` ON Nope (i)"),
            // One character shorter is a name both servers take, and both
            // report all 64 of it.
            same("CREATE TABLE Idok (`aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` INT)"),
            same("SELECT `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` FROM Idok"),
            same("SELECT COLUMN_NAME FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Idok'"),
            // Characters and not bytes: 64 two-byte characters are 128 bytes
            // and are taken, 65 of them are refused.
            same("CREATE TABLE Idw (`éééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééé` INT)"),
            same("CREATE TABLE Idw2 (`ééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééé` INT)"),
            // An alias is cut rather than refused, at 256 bytes.
            same("SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            same("SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            same("SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            // Bytes, not characters, which is the opposite of the limit
            // above: 129 two-byte characters come back as 128.
            same("SELECT 1 AS `ééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééé`"),
            // And the cut is what the alias *is*: the first 256 bytes resolve
            // where the name as written does not.
            same("SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` ORDER BY `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            same("SELECT 2 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` ORDER BY `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            // The shapes that carry a name out of an inner query: each takes
            // its column name from an item one level down, so the cut has to
            // have happened before the name is carried up.
            same("SELECT * FROM (SELECT 'x' AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`) d"),
            same("WITH c AS (SELECT 'x' AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`) SELECT * FROM c"),
            same("SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` UNION SELECT 2 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` ORDER BY 1"),
            // The kinds that carry no limit at all, so the refusal above is
            // not widened into them. A table alias and a CTE's column names
            // both take 300 characters on 8.3 and resolve in full.
            //
            // These materialise a string rather than an integer on purpose.
            // A derived table or a CTE over `SELECT 1` is MYSQL_TYPE_LONG
            // there and LONGLONG here — #0110, the width a materialised
            // column takes — which has nothing to do with how long a name may
            // be, and asking it here would cost the empty baseline for a
            // reason this group is not about.
            same("SELECT 1 FROM (SELECT 'x') AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`"),
            same("WITH c (`aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`) AS (SELECT 'x') SELECT * FROM c"),
            // A named constraint written against the *column* rather than the
            // table. The table-level spelling was refused while this one was
            // stored, over-long name and all, because the check read a
            // `TableConstraint` and this is a column option.
            same("CREATE TABLE Idc (i INT CONSTRAINT `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` CHECK (i > 0))"),
            same("ALTER TABLE Idok ADD COLUMN k INT CONSTRAINT `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` CHECK (k > 0)"),
            // An alias inside a subquery written in an expression rather than
            // in the FROM. A view stores it as a column name, so it is 1166
            // there; the same statement without the view answers, which is
            // what makes this the view path's business.
            same("CREATE VIEW Idv AS SELECT COALESCE((SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`), 1) AS y"),
            same("CREATE VIEW Idv2 AS SELECT 1 AS y FROM Idok WHERE 1 IN (SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`)"),
            same("CREATE VIEW Idv3 AS SELECT 1 AS y FROM Idok WHERE EXISTS (SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`)"),
            same("SELECT 1 AS y FROM Idok WHERE 1 IN (SELECT 1 AS `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa`)"),
        ],
    },
    Group {
        // An index name is scoped to its table, so the setup below claiming
        // `ix_one` on two of them is itself a case: if either server refused
        // it, the run says so as a failed setup rather than as agreement.
        // `IxAuto` is the same kind of claim about the names MySQL invents for
        // an index the statement did not name — two indexes on one column are
        // legal there because the second is called `a_2`.
        name: "an index name the table already carries",
        setup: &[
            // `c` is in no index, which is what makes the foreign key case
            // below create one: 8.3 gives a foreign key an index of its own
            // only where none already covers its columns, and one that creates
            // no index cannot collide with a name.
            "CREATE TABLE Ix (a INT NOT NULL, b INT NOT NULL, c INT NOT NULL, PRIMARY KEY (a))",
            "CREATE INDEX ix_one ON Ix (b)",
            "CREATE TABLE IxOther (a INT NOT NULL, PRIMARY KEY (a), INDEX ix_one (a))",
            "CREATE TABLE IxAuto (a INT NULL, b INT NULL, INDEX (a, b), INDEX (a), INDEX (a))",
            // `b` holds duplicates, so a unique index over it is refusable on
            // its rows as well as on its name — which is what the pair of
            // cases below asks the order of.
            "INSERT INTO Ix VALUES (1, 1, 1), (2, 1, 1)",
            // A primary key with a constraint name beside an index under that
            // same name, which 8.3 accepts because it does not call its key
            // that: another claim the setup makes by not failing.
            "CREATE TABLE IxPk (a INT NOT NULL, b INT NULL, CONSTRAINT mypk PRIMARY KEY (a), \
             INDEX mypk (b))",
            "CREATE TABLE IxDrop (a INT NULL, b INT NULL, INDEX ix (a))",
        ],
        cases: &[
            // The names both servers invented, and the one place the corpus
            // asks what an unnamed index is called: the first column, then a
            // suffix counted until it is free.
            same("SELECT TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX, COLUMN_NAME \
                  FROM information_schema.statistics WHERE TABLE_SCHEMA = DATABASE() \
                  AND TABLE_NAME = 'IxAuto' ORDER BY INDEX_NAME, SEQ_IN_INDEX"),
            same("SELECT TABLE_NAME, INDEX_NAME FROM information_schema.statistics \
                  WHERE TABLE_SCHEMA = DATABASE() AND INDEX_NAME = 'ix_one' \
                  ORDER BY TABLE_NAME"),
            // A duplicate inside one CREATE TABLE, in each of the spellings.
            // Every one of these is refused, so none of them leaves a table
            // behind for the next case to trip over.
            same("CREATE TABLE IxDup (a INT, INDEX i (a), INDEX i (a))"),
            same("CREATE TABLE IxDupKey (a INT, KEY i (a), KEY i (a))"),
            same("CREATE TABLE IxDupUnique (a INT, UNIQUE u (a), UNIQUE u (a))"),
            // A unique and a plain index share one name space, not two.
            same("CREATE TABLE IxDupMixed (a INT, INDEX i (a), UNIQUE i (a))"),
            // Compared case-insensitively.
            same("CREATE TABLE IxDupCase (a INT, INDEX i (a), INDEX I (a))"),
            // A column-level UNIQUE takes the column's name, and an index
            // written under that name collides with it.
            same("CREATE TABLE IxDupColumn (a INT UNIQUE, b INT, INDEX a (b))"),
            // An invented name is in the same space as a written one.
            same("CREATE TABLE IxDupAuto (a INT, b INT, INDEX (a, b), INDEX a (b))"),
            // Two foreign keys of one name, where it is the index each of them
            // creates that collides.
            same("CREATE TABLE IxDupFk (a INT NOT NULL, b INT NOT NULL, \
                  CONSTRAINT fk FOREIGN KEY (a) REFERENCES IxOther(a), \
                  CONSTRAINT fk FOREIGN KEY (b) REFERENCES IxOther(a))"),
            // A second CREATE INDEX under a name the table already carries.
            same("CREATE INDEX ix_one ON Ix (a)"),
            same("CREATE INDEX IX_ONE ON Ix (a)"),
            // The name is answered before the rows: 1061 for the taken name,
            // and 1062 for the same statement under a free one.
            same("CREATE UNIQUE INDEX ix_one ON Ix (b)"),
            same("CREATE UNIQUE INDEX ix_free ON Ix (b)"),
            // The same refusal reached through ALTER TABLE, which is where
            // Prisma's migrations write it.
            same("ALTER TABLE Ix ADD INDEX ix_one (a)"),
            same("ALTER TABLE Ix ADD UNIQUE INDEX ix_one (a)"),
            same("ALTER TABLE Ix ADD CONSTRAINT ix_one UNIQUE (a)"),
            same("ALTER TABLE Ix ADD CONSTRAINT ix_one FOREIGN KEY (c) REFERENCES IxOther(a)"),
            // The collision between two operations of one statement, which no
            // stored index is involved in at all.
            same("ALTER TABLE Ix ADD INDEX ix_two (a), ADD INDEX ix_two (b)"),
            // Nothing above was applied: the table still carries the two
            // indexes the setup gave it.
            same("SELECT INDEX_NAME, COLUMN_NAME FROM information_schema.statistics \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Ix' \
                  ORDER BY INDEX_NAME"),
            // The primary key's index is `PRIMARY` and not the constraint's
            // name, which is why the index beside it could take that name.
            same("SELECT INDEX_NAME, COLUMN_NAME FROM information_schema.statistics \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'IxPk' \
                  ORDER BY INDEX_NAME"),
            // A drop matches the name the way the duplicate check does, so
            // freeing a name under another spelling and reusing it in the same
            // statement works. Idempotent, which a case that changes anything
            // has to be: it runs once per protocol.
            same("ALTER TABLE IxDrop DROP INDEX IX, ADD INDEX ix (b)"),
            same("SELECT INDEX_NAME, COLUMN_NAME FROM information_schema.statistics \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'IxDrop' \
                  ORDER BY INDEX_NAME"),
        ],
    },
    Group {
        // `Dr` carries one of everything a DROP can name -- a column, a plain
        // index, a *unique* index, a foreign key, a check and a primary key --
        // because the question these cases ask is which name space each drop
        // reads, and a table with only columns on it cannot tell a shared
        // lookup from five separate ones. The unique index is the one that
        // separates `DROP CONSTRAINT` from `DROP INDEX`: it is in both spaces
        // where `ix` is in only one, and a corpus without it let this engine
        // ship a `DROP CONSTRAINT` that refused every UNIQUE.
        //
        // `DrChk` is built by the setup rather than declared, because what it
        // is for is the state only a *drop* can reach: a table whose check
        // suffixes have a hole in them.
        name: "a drop that names nothing",
        setup: &[
            "CREATE TABLE DrP (id INT NOT NULL, PRIMARY KEY (id))",
            "CREATE TABLE Dr (id INT NOT NULL, a INT NULL, b INT NULL, u INT NULL, \
             pid INT NULL, PRIMARY KEY (id), INDEX ix (a), CONSTRAINT uq UNIQUE (u), \
             CONSTRAINT fk1 FOREIGN KEY (pid) REFERENCES DrP(id), \
             CONSTRAINT ck1 CHECK (b > 0))",
            "CREATE TABLE DrNoPk (a INT NULL, b INT NULL)",
            "CREATE TABLE DrPk (id INT NOT NULL, a INT NULL, PRIMARY KEY (id))",
            // One name on two kinds of constraint, which 8.3 accepts making
            // and then refuses to drop by that name.
            "CREATE TABLE DrAmb (a INT NULL, b INT NULL, CONSTRAINT n UNIQUE (a), \
             CONSTRAINT n CHECK (b > 0))",
            "CREATE TABLE DrChk (a INT NULL, b INT NULL)",
            "ALTER TABLE DrChk ADD CHECK (a > 0)",
            "ALTER TABLE DrChk ADD CHECK (b > 0)",
            "ALTER TABLE DrChk DROP CONSTRAINT DrChk_chk_1",
            // `DrChk_chk_3` on 8.3, because the suffix counts past the highest
            // name present and not past how many checks there are. An engine
            // that counted would call this one `DrChk_chk_2` as well, and the
            // case below would not find a `_chk_3` to drop.
            "ALTER TABLE DrChk ADD CHECK (a > 5)",
        ],
        cases: &[
            // One code for every drop that names nothing, whichever name
            // space it reads. This engine used to answer the first two with
            // 1054 and to accept the next three in silence.
            same("ALTER TABLE Dr DROP COLUMN nosuch"),
            same("ALTER TABLE Dr DROP nosuch"),
            same("ALTER TABLE Dr DROP INDEX nosuch"),
            same("ALTER TABLE Dr DROP FOREIGN KEY nosuch"),
            // The standalone statement, which reaches a different code path
            // here than the ALTER TABLE form does.
            same("DROP INDEX nosuch ON Dr"),
            // No name is written, so the one reported is the one MySQL keeps a
            // primary key's index under.
            same("ALTER TABLE DrNoPk DROP PRIMARY KEY"),
            // The name spaces do not leak: `ix` is an index and `a` a column,
            // and each drop is blind to the other's.
            same("ALTER TABLE Dr DROP COLUMN ix"),
            same("ALTER TABLE Dr DROP INDEX a"),
            same("ALTER TABLE Dr DROP FOREIGN KEY ix"),
            same("ALTER TABLE Dr DROP FOREIGN KEY ck1"),
            // `DROP CONSTRAINT` reads a different space -- the one
            // TABLE_CONSTRAINTS lists, which holds no ordinary non-unique
            // index -- and misses with a code of its own rather than the one
            // above. `ix` is a plain index and `a` a column.
            same("ALTER TABLE Dr DROP CONSTRAINT nosuch"),
            same("ALTER TABLE Dr DROP CONSTRAINT ix"),
            same("ALTER TABLE Dr DROP CONSTRAINT a"),
            // A table with no primary key, so `PRIMARY` names nothing here
            // either -- unlike on `DrPk` below, where the same statement works.
            same("ALTER TABLE DrNoPk DROP CONSTRAINT `PRIMARY`"),
            // One name on two kinds of constraint: refused rather than
            // dropping both, and with a third code again.
            same("ALTER TABLE DrAmb DROP CONSTRAINT n"),
            // The line the drop code does not cross. An operation that *reads*
            // a column rather than removing one keeps the field-list code, on
            // the same table and the same absent name.
            same("ALTER TABLE Dr MODIFY COLUMN nosuch INT"),
            same("ALTER TABLE Dr RENAME COLUMN nosuch TO z"),
            // Which of two bad operations is reported. `DROP CONSTRAINT` is
            // settled in a phase ahead of the rest, so it wins from either
            // side; two operations from the same phase go in written order.
            same("ALTER TABLE Dr DROP COLUMN nosuch1, DROP CONSTRAINT nosuch2"),
            same("ALTER TABLE Dr DROP CONSTRAINT nosuch2, DROP COLUMN nosuch1"),
            same("ALTER TABLE Dr DROP COLUMN nosuch1, DROP INDEX nosuch2"),
            same("ALTER TABLE Dr DROP INDEX nosuch2, DROP COLUMN nosuch1"),
            // That phase is settled against the table as it stands, so an ADD
            // in the same statement is invisible to it from either side.
            same("ALTER TABLE Dr ADD CONSTRAINT c2 CHECK (b > 1), DROP CONSTRAINT c2"),
            same("ALTER TABLE Dr DROP CONSTRAINT c2, ADD CONSTRAINT c2 CHECK (b > 1)"),
            // The drops that are not in that phase run against a copy of the
            // table, so the second of a doubled pair finds the first has
            // already taken the name.
            same("ALTER TABLE Dr DROP COLUMN a, DROP COLUMN a"),
            same("ALTER TABLE Dr DROP INDEX ix, DROP INDEX ix"),
            same("ALTER TABLE Dr DROP FOREIGN KEY fk1, DROP FOREIGN KEY fk1"),
            // A valid operation beside a refused one is not applied, which the
            // two shape cases below are what actually check.
            same("ALTER TABLE Dr DROP COLUMN a, DROP COLUMN nosuch"),
            same("ALTER TABLE Dr DROP INDEX ix, DROP COLUMN nosuch"),
            // Nothing above reached the table: every column and every index
            // the setup gave it is still there.
            same("SELECT COLUMN_NAME FROM information_schema.columns \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Dr' \
                  ORDER BY ORDINAL_POSITION"),
            same("SELECT INDEX_NAME, COLUMN_NAME FROM information_schema.statistics \
                  WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'Dr' \
                  ORDER BY INDEX_NAME"),
            // The refusals are not bought by never finding anything: these
            // drops do reach their targets, under a spelling the setup did not
            // use. Idempotent, which a case that changes anything has to be --
            // each puts back the name it took.
            same("ALTER TABLE Dr DROP CONSTRAINT CK1, ADD CONSTRAINT ck1 CHECK (b > 0)"),
            same("ALTER TABLE Dr DROP INDEX IX, ADD INDEX ix (a)"),
            // The two `DROP CONSTRAINT` reaches that `DROP INDEX`'s space and
            // the plain refusals above cannot show: a unique index, and the
            // primary key under the literal name MySQL keeps it under. Both
            // were 3940 here until the name space was measured properly.
            same("ALTER TABLE Dr DROP CONSTRAINT UQ, ADD CONSTRAINT uq UNIQUE (u)"),
            same("ALTER TABLE DrPk DROP CONSTRAINT `PRIMARY`, ADD PRIMARY KEY (id)"),
            // Only reachable if the check the setup added last was named
            // `_chk_3`: an engine that numbered by counting rather than by the
            // highest suffix present would have called it `_chk_2` -- a second
            // check under a name already in use -- and would answer 3940 here.
            // The name is written back explicitly, so this says nothing about
            // numbering the second time round and stays idempotent.
            same("ALTER TABLE DrChk DROP CONSTRAINT DrChk_chk_3, \
                  ADD CONSTRAINT DrChk_chk_3 CHECK (a > 5)"),
            // On a table of its own, because dropping a column MySQL has an
            // index over drops the index too and this engine does not -- a
            // divergence of its own, and not one to drag into these cases.
            same("ALTER TABLE DrNoPk DROP COLUMN A, ADD COLUMN a INT NULL"),
        ],
    },
];
