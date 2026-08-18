# The working prototype

As of 2026-08-18 the engine runs a full Prisma stack end to end: `prisma db push` creates
a schema, and Prisma Client performs CRUD against it.

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

## What it does not do yet

Named honestly, because the engine returns an error rather than a wrong answer for each:
explicit window frames (`ROWS`/`RANGE BETWEEN`), named `WINDOW` clauses, `NTH_VALUE`, a
computed (non-constant) offset or default in `LAG`/`LEAD`, `ORDER BY` and `LIMIT` on an
`UPDATE` or `DELETE`, `DELETE ... USING`, and the state image that would make startup skip
migrations entirely.

Two limitations are worth stating rather than implying away.

**Transactions do not isolate concurrent connections.** The catalog is shared, so a second
connection sees uncommitted writes. Rollback restores only the tables the transaction
actually wrote, so another connection's work on *other* tables survives — but if both
wrote the same table, the rollback discards the other's committed rows too. That is
correct enough for the fresh-process-per-test-file model this engine is built for, and
wrong for anything else.

**Rollback keeps a catalog snapshot rather than an undo log**, which is the honest trade
for an in-memory engine holding a test-sized dataset; undo logging would be a great deal of
machinery for a database that discards everything at process exit.

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
dropped, so `DELETE ... LIMIT 1` emptied the table — all four are refused now; the
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
