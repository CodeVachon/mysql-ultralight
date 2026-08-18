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
- **Sessions** — system variables including the connection preamble every driver reads,
  user variables that outlive the statement that set them, `SET`, `SHOW`
- **`information_schema`** — `SCHEMATA`, `TABLES`, `COLUMNS`, `STATISTICS`,
  `TABLE_CONSTRAINTS`, `KEY_COLUMN_USAGE`, `REFERENTIAL_CONSTRAINTS`, and the empty tables
  Prisma expects to exist, all queryable and joinable as ordinary tables

## What it does not do yet

Named honestly, because the engine returns an error rather than a wrong answer for each:
`GROUP BY`, `CHECK` constraint enforcement, generated columns, views, subqueries in
expressions, `UNION`, window functions, `PREPARE`/`EXECUTE` dynamic SQL, real transactions
(`BEGIN`/`COMMIT` are accepted but nothing rolls back), foreign key enforcement, and the
state image that would make startup skip migrations entirely.

## Review hardening

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

## Two bugs worth remembering

**Temporal values need the packed binary form.** Sent as a length-encoded string over the
prepared-statement protocol, Prisma's driver reads the string's length byte as a year and
panics. Every Prisma Client read failed until `DATE`/`DATETIME`/`TIME` were encoded
properly; `count` kept working throughout, because integers were fine.

**`SELECT DISTINCT` was silently ignored**, returning four identical rows where one was
correct. It surfaced only because a test asserted the row count. This is the failure mode
the project exists to avoid, and it is why unimplemented features error here rather than
degrade.
