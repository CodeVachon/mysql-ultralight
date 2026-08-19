# Changelog

## 0.1.0

The first release worth using. It runs a real application's database: all **604
migrations** of a production Prisma history apply, and a server booted from an
image baked from them answers `prisma migrate deploy` with "No pending
migrations to apply".

| | Real MySQL 8 | mysql-ultralight |
|---|---|---|
| Start | 4.5–8.9s of container | 0.1 ms |
| Schema | 26.1s of `prisma migrate deploy` | 1.3–1.7 ms from a baked image |

### The state image

`SELECT ultralight_snapshot('schema.mus')` writes the whole catalog to a file
and `--state schema.mus` boots from it, which is what turns a 26-second
migration replay into about a millisecond per test process.
`ultralight_restore` does the same to a running server, resetting it between
test files without starting a new one.

The format is versioned and hand-rolled. A stale, truncated, foreign or
trailing-bytes image is refused rather than half-read, because a catalog with
the wrong shape would make every test wrong in a way nothing reports.

### The npm package

`@codevachon/mysql-ultralight` ships a programmatic API and the CLI:

```js
const db = await start({ image: '.cache/schema.mus' });
process.env.DATABASE_URL = db.url;
await db.stop();
```

Port 0 by default, so parallel test workers cannot race for one, and `start()`
resolves only once the server is accepting connections. Prebuilt binaries for
macOS arm64 and Linux x64/arm64, glibc and musl.

### SQL

Joins, `GROUP BY` with functional dependency, window functions with real frame
semantics, CTEs, set operators with multiset `ALL`, subqueries, views, foreign
keys with cascades, transactions with savepoints, generated columns, `CHECK`
constraints, multi-table `UPDATE`/`DELETE`, prepared statements,
`information_schema`, `sql_mode` handling, JSON with `JSON_TABLE`, and
`ON DUPLICATE KEY UPDATE`.

What is not supported is refused with a clear error rather than answered
wrongly — the one failure mode a test database must not have. Currently:
explicit window frames, named `WINDOW` clauses, `NTH_VALUE`, a computed
`LAG`/`LEAD` offset, `ORDER BY`/`LIMIT` on DML, `DELETE ... USING`,
`WITH RECURSIVE`, `NESTED PATH` in `JSON_TABLE`, the date-arithmetic functions,
and the spatial pair.

One known gap is *not* an error, and is recorded rather than hidden: nested JSON
constructors double-encode, because JSON is held as text. See `docs/prototype.md`.

### Not for production

There is no durability and there never will be. It is a test database.
