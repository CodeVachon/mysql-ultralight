# The working prototype

As of 2026-08-18 the engine runs a full Prisma stack end to end: `prisma db push` creates
a schema, and Prisma Client performs CRUD against it.

The numbered measurements below are that run and are left as they were taken. The
capability lists are kept current; where a claim here has since been checked against a
real `mysql:8.3` rather than reasoned about, the date of the measurement is given, because
in this repository the reasoned-about ones have been wrong often enough to be worth
distinguishing. `scripts/ask.sh` is how they get checked.

```
$ mysql-ultralight --port 3307 --database testdb
ready port=3307 socket=- startup_ms=0.431

$ DATABASE_URL="mysql://test:test@127.0.0.1:3307/testdb" npx prisma db push
🚀  Your database is now in sync with your Prisma schema. Done in 8ms
```

| Prisma Client operation | Result |
|---|---|
| `create` | works |
| `findMany` with `orderBy` | works |
| `findUnique` | works |
| `where` with `gt` | works |
| `where` with `contains` | works |
| `count` | works |
| `update` | works |
| `findMany` with `take` / `skip` | works |
| `delete` | works |

**Cold start: 2.05ms median** from process spawn to a client holding the handshake, over 20
runs (min 1.83ms, p95 2.59ms). The equivalent path on real MySQL is 4.5–8.9s of container
start plus 26.1s of `prisma migrate deploy`. See [`baseline-mysql.md`](baseline-mysql.md).

## What the engine does

- **DDL** — `CREATE`/`DROP DATABASE`, `CREATE TABLE` with typed columns, defaults, primary
  keys, unique and plain indexes, foreign keys recorded for introspection; `ALTER TABLE`
  add/drop/rename/modify column, add/drop constraint; `CREATE INDEX`; `TRUNCATE`; `USE`
- **DML** — `INSERT` (multi-row, `IGNORE`, column defaults, auto-increment), `UPDATE`,
  `DELETE`, all with `WHERE`
- **Queries** — projection, `WHERE`, `ORDER BY` (including over columns the SELECT list
  omits), `LIMIT`/`OFFSET`, `DISTINCT`, `INNER`/`LEFT`/`RIGHT`/`CROSS JOIN` with `ON` and
  `USING`, derived tables, aggregates (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `GROUP_CONCAT`)
- **Expressions** — comparison with correct three-valued NULL logic, `AND`/`OR`, arithmetic,
  `BETWEEN`, `IN`, `LIKE` with escapes, `CASE`, `CAST`, `IS NULL`, and about 50 scalar
  functions
- **Grouping** — `GROUP BY` over several columns, `HAVING`, aggregates folded inside larger
  expressions such as `SUM(a) / COUNT(*)`, `COUNT(DISTINCT x)`, and NULLs grouped together
- **Set operators** — `UNION`, `INTERSECT` and `EXCEPT`, each with `ALL`
- **JSON** — `JSON_EXTRACT` and the `->`/`->>` operators, `JSON_OBJECT`, `JSON_ARRAY`,
  `JSON_SET`, `JSON_MERGE_PATCH`, `JSON_ARRAYAGG`, `JSON_TABLE` with `FOR ORDINALITY`, and
  the path syntax they address with
- **Multi-table DML** — `UPDATE a JOIN b SET ...` and `DELETE a FROM a JOIN b`, writing back
  through the join to the rows it came from
- **Window functions** — `ROW_NUMBER`, `RANK`, `DENSE_RANK`, `LAG`, `LEAD`, `FIRST_VALUE`,
  `LAST_VALUE` and aggregates over `OVER (PARTITION BY ... ORDER BY ...)`
- **Subqueries** — scalar, `EXISTS`, `IN (SELECT ...)`, and correlated forms that reference
  the enclosing query's columns
- **Views** — `CREATE`, `CREATE OR REPLACE`, `DROP`, materialised on read so they reflect
  later writes, queryable and joinable like tables
- **Dynamic SQL** — `SET @x := ...`, `PREPARE`, `EXECUTE ... USING`, `DEALLOCATE`: the
  idiom the migrations use for conditional DDL
- **`sql_mode`** — `ANSI_QUOTES`, `PIPES_AS_CONCAT` and `IGNORE_SPACE` change how a
  statement is parsed and what it means
- **Constraints** — primary and unique keys, `CHECK`, `NOT NULL`, generated columns
  computed from the row, and foreign keys enforced on write with `CASCADE`, `SET NULL` and
  `RESTRICT` on delete, cascading through chains
- **Transactions** — `BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT` and `ROLLBACK TO SAVEPOINT`,
  with writes and schema changes genuinely undone
- **Sessions** — system variables including the connection preamble every driver reads,
  user variables that outlive the statement that set them, `SET`, `SHOW`
- **`information_schema`** — `SCHEMATA`, `TABLES`, `COLUMNS`, `STATISTICS`,
  `TABLE_CONSTRAINTS`, `KEY_COLUMN_USAGE`, `REFERENTIAL_CONSTRAINTS`, and the empty tables
  Prisma expects to exist, all queryable and joinable as ordinary tables
- **Recursive CTEs** — `WITH RECURSIVE`, run rather than refused
- **`RENAME TABLE`** — several pairs at once, applied in order so a chained rename through
  a temporary name works, and all-or-nothing if any pair is bad. Renaming a table *into
  another database* is refused with a clear 1235 rather than performed (#0251); it
  previously built a table literally named `other.Alt` in the current database, whose rows
  no qualified name could reach
- **Types held to their declarations** — a write is refused when the declared type cannot
  hold it, integers and decimals are materialised at the width and scale they were declared
  with, and a `DECIMAL` keeps its own value rather than a float's idea of it
- **Collations** — a string is folded and compared under the collation its column was
  declared with, and a name is compared as bytes
- **Prepared statements resolve** — `COM_STMT_PREPARE` settles a missing table, a missing
  column, and the names a write uses, at prepare, the way 8.3 does, rather than deferring
  every error to execution. A parameter is typed from its position
- **Diagnostics** — a warning can be raised and read back, and `ROW_COUNT()` reports rows
  changed or rows matched according to what the client negotiated

- **Identifier limits** — an identifier longer than MySQL allows is refused, at the
  lengths MySQL uses

One behaviour is a deliberate divergence rather than a gap, and it is a refusal:

**A statement is refused above 256 levels of nesting**, with 1436 `Thread stack overrun`,
where 8.3 answers deeper ones. Every walk over a statement — the evaluator, the walkers
that type a column and decide its nullability — recurses once per node, so a deep enough
statement took down the process and every other connection with it. A flat chain costs the
same as a hand-written nest, because `a OR b OR c` *is* a tree of depth three; treating the
two differently was tried and had to be reverted, because the separate bounds composed into
a shape that passed both and still aborted. The number is measured against the 32 MB stack
the binary gives each connection, about five times under where a `cargo test` build stops
answering. Driven as a *library* on a smaller thread the margin is not there — #0253.

## Parser coverage against the real corpus

The conformance extractor can measure the engine's front end as well as sqlparser-rs alone
(`--engine`). Against the same 4,661 statements:

| | sqlparser-rs alone | mysql-ultralight front end |
|---|---|---|
| Migrations | 94.6% | **100.0%** (3,161 of 3,161) |
| Provisioning dumps | 100% | 100% |
| Application raw SQL | 99.0% | 99.0% |
| **Total** | **96.3%** | **99.9%** |

Three statements remain, all artifacts of recovering SQL from `Prisma.sql` templates, where
an interpolation stands for a whole clause rather than a value — the complete statement
never exists in source, so nothing can parse it. **No statement in the 604 migrations or the
provisioning dumps is rejected.**

The last genuine gap was an `IN` list whose elements are scalar subqueries, which MySQL
accepts and sqlparser-rs reads as `IN (subquery)` before failing on the comma. It is
rewritten into `IN ((SELECT a) UNION ALL (SELECT b))`, which is equivalent and which the
engine already supports. The branches keep their parentheses deliberately: a branch may end
in `LIMIT`, and `SELECT ... LIMIT 1 UNION ALL SELECT ...` binds the LIMIT to the whole
union and stops parsing.

## Running the real migrations, not just parsing them

Parsing every statement and *running* every statement are different claims, and only the
second one matters. Pointing `prisma migrate deploy` at the engine with the real
604-migration history now applies **all 604**, and reports "No pending migrations to
apply" when run again against the image baked from it.

Everything between the two numbers was found this way, one migration at a time. Some of it
was missing features — multi-statement commands (Prisma sends a whole migration file as one
command), CTEs, `INSERT ... SELECT`, named locks, `UUID`, `RAND`, `MD5`, the `REGEXP`
family, `DROP INDEX`, `VALUES ROW(...)`, `CAST AS SIGNED`, `ON UPDATE CURRENT_TIMESTAMP`.
The rest were bugs no unit test had reason to write:

- **Every writer deadlocked on a subquery.** `INSERT`, `UPDATE` and `DELETE` held the
  catalog's write lock while evaluating, and a subquery in `VALUES`, `SET` or `WHERE`
  reaches the executor, which takes a read lock to answer it. `RwLock` is not reentrant, so
  the connection waited on a lock it held itself, forever. Writers now compute against a
  copy with no lock held and swap it in at the end.
- **A foreign key created no index.** MySQL indexes every foreign key and names the index
  after the constraint; Prisma renames that index in a later migration, which had nothing
  to find.
- **An inline `UNIQUE INDEX name (...)` lost its name**, because the parser puts it in a
  different field from `CONSTRAINT name UNIQUE (...)`. Every one was stored under an
  invented name, so `information_schema` reported the wrong thing and `DROP INDEX` failed.
- **`UPDATE Board b ... WHERE b.x` did not resolve the alias** in the single-table path.
- **`ONLY_FULL_GROUP_BY` rejected valid queries**: MySQL permits a column that the grouping
  functionally determines, which a join equality can establish.
- **The first deterministic `UUID()` put its counter last**, so every value shared a
  24-character prefix — and the migrations derive ids with
  `SUBSTRING(REPLACE(UUID(), '-', ''), 1, 21)`. Every one collided. The counter is in the
  leading field now, where MySQL's time-low is.

The review of that work found twelve more, all fixed. The worst was introduced by the
deadlock fix itself: **writers computed against a copy with no lock held, so two
connections discarded each other's rows and both reported success.** Four threads inserting
200 rows each left 325 of 800 present, with no error anywhere. Writers now hold a gate
across the whole copy-compute-commit cycle — the catalog's write lock still cannot be held
while evaluating, but only one writer runs at a time. Two regression tests reproduce the
original loss.

Three others were wrong answers rather than gaps: `ONLY_FULL_GROUP_BY` propagated
functional dependency in *both* directions through an outer join, so
`a LEFT JOIN b ON a.id = b.id GROUP BY b.id` permitted `a.id` and returned an arbitrary row
from the unmatched group; `RAND()` ran a bare counter through one xorshift round and
returned values that agreed to nine decimal places, so `ORDER BY RAND()` did not shuffle
and `WHERE RAND() < 0.5` matched every row or none; and `CAST('12abc' AS SIGNED)` returned
NULL where MySQL returns 12.

## The state image

The point of the whole project, and the one number that justifies it.

`prisma migrate deploy` over the real history costs 26.1s, and a suite that starts a fresh
process per test file pays it every time. Baking the result once and loading it costs
**about a millisecond**:

```sh
mysql-ultralight --port 3306 &                       # 0.1ms, empty
npx prisma migrate deploy                            # once, 13.6s here
mysql -e "SELECT ultralight_snapshot('schema.mus')"  # 5ms, 918 KB

mysql-ultralight --state schema.mus                  # 1.3–1.7ms, every test process
```

The image carries the whole catalog — 278 tables, 2,771 columns, seed rows, foreign keys,
indexes, generated columns, `CHECK` constraints and auto-increment counters — and a server
booted from one is indistinguishable from one that ran the migrations: the constraints
still bite, and `prisma migrate deploy` against it answers **"No pending migrations to
apply"**.

`SELECT ultralight_restore('schema.mus')` does the same to a *running* server, which resets
it between test files without paying for a new process at all.

The format is hand-rolled for the same reason the wire codec is. Field order is the format,
so nothing may be reordered and a new field means a new version number. A version mismatch,
a truncation, a foreign file and trailing bytes are all refused: reading a stale image
"successfully" would produce a catalog with the wrong shape and no complaint, and every
test running against it would be wrong in a way nothing reports. The image is written to a
neighbouring temporary and renamed, so a reader can never see a half-written one.

## The bug the image found

Booting from an image was the first time anything asked this engine for a table it had not
just created, and `prisma migrate deploy` promptly failed with `Table '_prisma_migrations'
already exists` — after an introspection query that had returned that very table.

**`COM_STMT_PREPARE` declared zero columns.** A client caches the column metadata from the
prepare response and reads every later execution against it, so declaring nothing there
costs the result its column *names*. Prisma reads `table_name` out of that introspection by
name, found nothing, and concluded the table was missing. `SELECT 1 AS alpha` came back as
`f0`.

Nothing caught it because Prisma Client maps columns by position using its own schema — the
migration engine is the first consumer that reads a raw result by name. The prepare response
now describes the statement first: it is parsed, refused if it could write, run with every
parameter bound to NULL, and the resulting columns declared. The text protocol always
carried names correctly, which is why this hid for so long; both paths are now tested
directly at the wire level.

## Review hardening (state image)

Thirteen findings, all fixed. Two would have been serious in ordinary use:

- **A snapshot inside a write deadlocked the whole server.** Writers hold a global,
  non-reentrant gate across their copy-compute-commit cycle, so
  `INSERT INTO t VALUES (ultralight_snapshot('x'))` waited on a lock it already held —
  and because the gate is global, every other connection wedged behind it. Re-entering it
  is now an error that names the problem.
- **Describing a statement ran it, and a `SELECT` is not automatically free of
  consequences.** `COM_STMT_PREPARE` of `SELECT ultralight_restore('x')` replaced the live
  catalog without executing anything. The refusal is at the point of the call rather than
  a scan of the statement text, because a call hides just as easily inside a view's
  definition or a CTE.

Two more were wrong answers rather than hazards: a snapshot taken inside a transaction
baked uncommitted rows into the image permanently, and a restore inside one left `ROLLBACK`
replaying pre-transaction tables over a catalog that had been replaced wholesale, landing
somewhere that was neither the image nor where it started. Both are refused inside a
transaction now. Describing also consumed a `UUID()` or `RAND()` from the engine's
counters, so the same test produced different generated ids depending only on whether the
client used a prepared statement.

The rest: a failed snapshot left its temporary file behind and reported the temporary's
path rather than the one asked for; two bake processes in one directory shared a temporary
name; `from_catalog` silently kept an empty catalog if the lock was poisoned; and the image
readers built structs whose field evaluation order *was* the format, so a tidy-up that
reordered them would have changed the format with no compile error and no test failure.

What the review found correct is worth recording too: every field of every catalog type
round trips, and 200,000 randomly mutated or truncated images produced zero panics and only
clean errors.

## Review hardening (JSON and upserts)

Fifteen findings, all fixed but one, which is recorded below instead. The worst was a
silent cross join: **`JOIN JSON_TABLE(...) ON <cond>` ignored the condition entirely**,
because the lateral path read the join operator only to decide whether it was outer.
`ON jt.k = 2` returned every row, `ON 1 = 0` returned every row, and a `RIGHT JOIN` — which
the ordinary path refuses — quietly behaved as an inner one.

Four more were wrong answers in the JSON layer: `JSON_EXTRACT` returned an empty array
where MySQL returns NULL, so `IS NULL` was false for a path that matched nothing; `$[*]`
did not treat a scalar or object as a one-element array the way `$[0]` already did, so a
`JSON_TABLE` over an object produced no rows and the row vanished from the join; a JSON
`null` collapsed into SQL NULL, making "the key holds null" indistinguishable from "there
is no such key"; and whitespace between path legs — `$. a`, which MySQL accepts — became
part of the member name and matched nothing.

On the upsert side: `ON DUPLICATE KEY UPDATE` reported an insert id for a row it had not
inserted, so a client reading it back got a phantom; it updated the row a *secondary* key
named when the primary key also collided; `VALUES(col)`, the ordinary way to spell an
upsert, was unsupported; and `JSON_ARRAYAGG` dropped NULLs, applied a `DISTINCT` MySQL does
not accept, and returned `[]` rather than NULL for no rows.

`JSON_TABLE` column clauses that change the answer — `DEFAULT ... ON EMPTY`, `ERROR ON
EMPTY`, and a wildcard in a column path — were parsed and dropped, and are now refused
alongside the `EXISTS` and `NESTED PATH` that already were.

### The one that took a new value type

**Nested JSON constructors double-encoded.** `JSON_OBJECT('a', JSON_ARRAY(1, 2))` yielded
`{"a": "[1,2]"}` where MySQL yields `{"a": [1, 2]}`. JSON was held as text, so a JSON
result was an ordinary string by the time it reached the next function and was embedded as
one. Nothing distinguished "a document" from "a string that looks like one", which meant it
could not be detected and refused either — the one place the engine could be silently wrong
rather than clearly unsupported.

`Value::Json` is that distinction: still the serialised text, but tagged. Everything that
builds a document returns one, a `JSON` column coerces to one, and `CAST(... AS JSON)`
parses rather than relabels. A string stays a string — guessing by whether it parses would
turn a column holding `[9,9]` into an array nobody asked for. The tag reaches the state
image as value tag 5, appended rather than inserted, which is what took `FORMAT_VERSION`
to 2.

It also made a second thing possible: a `JSON` column now validates on the way in, as
MySQL does, instead of accepting `{"a":1` and failing every later read of it.

## The example, and what it found

`examples/app` is a worked Prisma application the gate runs: its own schema,
four migrations including an `ALTER` and a data migration, and the controllers
a suite would exercise. Three runner examples — `node-test`, `jest` and
`vitest` — contain nothing but tests, and all three test that same app. It exists because the
layers were only ever tested separately — the seam where a real driver's
expectations meet the wire protocol and the SQL had no coverage at all.

It found three engine bugs in its first hour, none reachable from a unit test:

- **`information_schema` reported numbers as text.** Prisma's schema describer
  reads `seq_in_index` as an integer and unwraps it, so `prisma db pull`,
  `migrate dev` and `migrate diff` crashed outright against this engine. They
  work now: `db pull` reads the real 278-table schema back as 209 models and
  66 enums.
- **A binary string was true whenever it was non-empty**, so `BINARY '0'` was
  true and Prisma's foreign-key query — which compares `BINARY a = BINARY b` —
  matched every pair. A four-row answer came back as sixty-four, silently. The
  fix is that a binary string is read for its leading number, as a string is.
- **`NOW(3)` ignored its precision**, so a `DATETIME(3)` column filled by it
  held whole seconds, and two rows written in the same second compared equal.

Two more came out of describing columns properly: `SUM` and `MAX` were reported
as text rather than the type they fold, and a view's columns were described with
no type at all.

The review of the example itself was as useful as the example. Four of its tests
passed for the wrong reasons — a rollback test that held just as well against an
engine that never wrote anything, an excerpt assertion that could not fail, a
schema comparison that omitted the defaults and keys it claimed to cover and
would have passed on two empty results, and a tautology on an auto-increment id.
Each is now pinned to an exact value, and the rollback test was checked by
breaking `ROLLBACK` and watching it fail.

## What it does not do yet

Named honestly, because the engine returns an error rather than a wrong answer for each:
explicit window frames (`ROWS`/`RANGE BETWEEN`), named `WINDOW` clauses, `NTH_VALUE`, a
computed (non-constant) offset or default in `LAG`/`LEAD`, and `NESTED PATH` in
`JSON_TABLE`.

`WITH RECURSIVE` was on that list until #0192 and is not any more.

`ORDER BY` and `LIMIT` on an `UPDATE` or `DELETE` are refused only on the multi-table
forms, where MySQL refuses them too — "the first row" is not defined across a join.

Three limitations are worth stating rather than implying away.

**Transactions do not isolate concurrent connections.** The catalog is shared, so a second
connection sees uncommitted writes. Rollback restores only the tables the transaction
actually wrote, so another connection's work on *other* tables survives — but if both
wrote the same table, the rollback discards the other's committed rows too. That is
correct enough for the fresh-process-per-test-file model this engine is built for, and
wrong for anything else.

**Rollback keeps a catalog snapshot rather than an undo log**, which is the honest trade
for an in-memory engine holding a test-sized dataset; undo logging would be a great deal of
machinery for a database that discards everything at process exit.

**A computed column has no declared type.** `SELECT id, name FROM t` through a view is
described exactly as the table is, because each column carries the type it was declared
with. `SELECT UPPER(name) AS up` is not: the engine infers nothing about an expression's
width, so the view reports `varchar(512)` where MySQL would say `varchar(40)`. The number
is the widening every computed column has always had rather than anything about this
expression, and a describing tool reads it as a given — so it is worth knowing before
building a view whose columns are all computed.

A `UNION` column used to be the same shape and mostly is not any more. Measured against
8.3 on 2026-09-08: `VARCHAR(10)` against `VARCHAR(20)` is described identically by both,
and so is `VARCHAR(10)` against itself, so differing widths are aggregated rather than
widened away. What is left is the display width on a narrow numeric union — `SMALLINT`
against `TINYINT` gives the right type on both sides and a length of 20 here against 6
there, which is `BIGINT`'s width rather than `SMALLINT`'s (#0254). A union mixing a string
with a number still falls to `VAR_STRING` where MySQL keeps `CHAR` (#0127).

## Review hardening (engine)

A high-effort review of the engine found thirteen issues, all fixed with regression tests.
Three were reproduced as outright defects a client could trigger:

- **Two panics reachable from a client.** The binary DATETIME and TIME encoders counted
  characters and then measured bytes, so a fraction containing any multi-byte character
  overflowed the scale subtraction. Storing junk in a DATETIME column and selecting it
  through a prepared statement killed the connection task.
- **`ALTER TABLE … RENAME TO x` named the table `TO x`.** The rename target's `Display`
  renders the keyword too. It also removed the old table by the name the user typed rather
  than the stored key, so a case difference left the rows present under both names.
- **A column-level `UNIQUE` was recorded but never enforced**, while
  `information_schema` reported `COLUMN_KEY = 'UNI'` — a constraint that existed only in
  the metadata.

The rest were the same family: `INSERT` silently truncating a `VALUES` row of the wrong
width, `CREATE UNIQUE INDEX` registering over data that already violated it, `ADD COLUMN …
NOT NULL` back-filling NULL, an explicit id not advancing the auto-increment counter, the
parameter binder and the placeholder counter disagreeing about `#` comments, `LIKE` not
honouring MySQL's default backslash escape, `COM_RESET_CONNECTION` discarding the session's
schema, `USE information_schema` failing on a schema the server advertises, the connection
id being assigned twice and able to disagree, and string comparison being byte-exact while
the server advertised a case-insensitive collation.

## Review hardening (blockers)

A second high-effort review of the blocker work found thirteen more, all fixed with
regression tests. Three shared one cause worth naming: `rewrite.rs` had three separate
scanners that understood quotes but not comments, and each grew its own bug from it — a
stray `"` in a comment swallowed the rest of the statement under ANSI_QUOTES, a `--`
comment absorbed the following line when IGNORE_SPACE closed up the newline, and an
apostrophe in a comment suppressed the `:=` rewrite. There is one scanner now.

The worst in isolation: `ALTER TABLE t RENAME INDEX a TO b, ADD COLUMN c INT` was
intercepted, renamed the index, reported success, and silently dropped the ADD COLUMN.
Silent schema divergence is the worst outcome a migration can have. The intercept now
requires exactly the eight-word form and otherwise falls through to the real parser.

The rest: views reported every column as text rather than its real type, were missing from
`information_schema.COLUMNS` and `SHOW TABLES`, and could have their name taken by a later
`CREATE TABLE` that would then be permanently unreachable; a correlated subquery with no
`FROM` lost the outer row; and every `SELECT` deep-copied its row set to support a
widening only correlated subqueries need.

## Review hardening (IN lists)

Six findings on the rewrite, all fixed. One was a process-killing panic: the check for
"is this element a parenthesised SELECT" sliced the first six *bytes*, which aborts when a
multi-byte character straddles the boundary. It needed no `SELECT` at all —
`WHERE (a,b) IN (('日本', 1), ('x', 2))` was enough, and with `panic = "abort"` in the
release profile that takes the whole server down rather than one connection.

Two more turned valid SQL invalid: the element test never checked that the leading and
trailing parentheses were *matched*, so `(SELECT a) + (b)` looked like a subquery, and the
keyword test had no word boundary, so `(selected)` did too. A third left a nested list
unexpanded, because the success path jumped past the branches it had just emitted.

The reviewer also caught a third hand-rolled literal-and-comment scanner in a module whose
own docstring says there is one. There is now genuinely one: `segments()` and the IN-list
pass are both built on a single `classify` primitive.

## Review hardening (constraints)

Twelve findings on this work, all fixed. Four were serious:

- **`ON DELETE RESTRICT` deleted the parent anyway.** The rows were written before the
  cascade ran, so the constraint reported an error against a row that was already gone.
  The existing test only asserted the error code, never that the row survived. DELETE and
  UPDATE now apply to a copy that is swapped in only if the whole cascade succeeds.
- **The cascade depth cap counted tables, not levels.** A hub with twenty referencing
  tables tripped a limit meant for a twenty-deep cycle — and, because of the bug above,
  did so after deleting the parent.
- **`ROLLBACK` discarded another connection's committed work**, because it replaced the
  whole shared catalog. It now restores only the tables the transaction wrote.
- **`UPDATE` never enforced the parent side of a foreign key**, so changing a parent key
  left children pointing at a value that no longer existed.

The rest: a self-referencing multi-row `INSERT` could not see its own earlier rows;
`INSERT IGNORE` turned a foreign key violation into a hard error instead of skipping the
row; `SET NULL` wrote NULL into `NOT NULL` columns and poisoned the row; inline
column-level `CHECK` was still refused; the 1451 message printed the constraint name where
MySQL prints the child table; and a stray `SAVEPOINT` under autocommit opened a
transaction, so a later `ROLLBACK` undid committed work.

## Review hardening (multi-table DML)

Twelve findings on this work, all fixed. The worst was unrecoverable data loss on a
statement the real server refuses: **`DELETE FROM a JOIN b` emptied both tables.** With no
explicit target list the code fell back to "every table in the join", reported two rows
affected and destroyed the lot. MySQL rejects that syntax outright, so it is an error here
now.

Three more came from the same root cause — the relation recorded only the *alias* each row
was addressed by, and a write needs the *base table* it came from:

- **A joined UPDATE or DELETE through an alias found nothing**, because the alias was used
  as a catalog key. `DELETE c FROM Child c JOIN Parent p ...` reported "table 'c' doesn't
  exist". The relation now carries `sources` (what the SQL names a row by) and
  `base_tables` (where the row lives) side by side; a self-join is only expressible with
  aliases, so both are needed.
- **A joined DELETE cascaded between removals**, so the row indices captured for the
  remaining tables had already shifted and the wrong rows were destroyed. Every named
  table's own rows are removed first, and only then do the cascades run.
- **A joined UPDATE applied each assignment to a separately re-read row**, so of
  `SET a = 1, b = 2` only the last one survived. All assignments now build one candidate
  row, which is validated and written once.

The rest: a joined UPDATE enforced no foreign keys and ran no `ON UPDATE` cascade at all;
it counted affected rows per *assignment* rather than per row; an ambiguous unqualified
`SET` target picked an arbitrary table instead of reporting 1052; a write to a generated
column was only caught if some row happened to match; `INTERSECT ALL` and `EXCEPT ALL` used
set membership where MySQL uses multiset arithmetic, so duplicate counts came out wrong;
and a computed `LAG`/`LEAD` offset silently became 1, answering a different question than
the one asked.

A second review of the fixes found ten more, all fixed. The worst repeated the first
pass's mistake one level down: **two aliases of one table are two join sources over a
single row vector**, so deleting per source applied the second set's pre-deletion indices
to an already-shortened table. `DELETE x, y FROM t x JOIN t y ON x.id = 1 AND y.id = 3`
deleted ids 1 and *4*. The sets are merged by base table now. The same confusion let a
join whose sources share a name — `DELETE t FROM t2 t JOIN t` — resolve to the first
match and delete from `t2`; MySQL's 1066 covers that, and both joined paths raise it.

Two were wrong answers this work made reachable rather than introduced:

- **An out-of-scope qualifier fell back to an unqualified lookup.** `WHERE zz.v = 100`
  bound to some other table's `v` and returned rows. Harmless-ish while only reads used
  it; with joined UPDATE and DELETE it drives writes. Both the evaluator and the
  projection planner now report 1054, as MySQL does.
- **A windowed aggregate with `ORDER BY` folded the whole partition.**
  `SUM(v) OVER (ORDER BY id)` returned the grand total on every row where MySQL returns a
  running one. The default frame is `RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW`,
  and in RANGE mode "current row" means the last row of the peer group — so tied keys
  share a total, and `LAST_VALUE` reads the last peer rather than the current row.

The rest: `LIMIT`, `ORDER BY` and `USING` on an `UPDATE` or `DELETE` parsed and were
dropped, so `DELETE ... LIMIT 1` emptied the table — all three are implemented now, and
refused on the multi-table forms where MySQL refuses them too; the
generated-column check ran only for the first assignment to a table, so
`SET note = 'q', shout = 'NO'` wrote nothing and said nothing; a self-join `UPDATE`
counted one physical row once per alias; a negative `LAG` offset quietly behaved as
`LEAD`; and `DELETE a.* FROM a JOIN b`, the canonical MySQL spelling, failed at the
parser — the `.*` is now stripped before parsing.

## Two bugs worth remembering

**Temporal values need the packed binary form.** Sent as a length-encoded string over the
prepared-statement protocol, Prisma's driver reads the string's length byte as a year and
panics. Every Prisma Client read failed until `DATE`/`DATETIME`/`TIME` were encoded
properly; `count` kept working throughout, because integers were fine.

**`SELECT DISTINCT` was silently ignored**, returning four identical rows where one was
correct. It surfaced only because a test asserted the row count. This is the failure mode
the project exists to avoid, and it is why unimplemented features error here rather than
degrade.
