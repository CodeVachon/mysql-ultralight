# Track C — Rust wire protocol viability

**Status:** proven. Prisma 5.4.2 connects, queries, and runs `prisma db push` against a
from-scratch Rust MySQL server.

The spike existed to answer one question: can a hand-written Rust server satisfy Prisma's
client without MySQL's source? It can, and the exercise produced a precise map of what
Prisma demands — which is worth more than the yes.

## Results

| Check | Result |
|---|---|
| Handshake + capability negotiation | Pass |
| `mysql_native_password` auth | Pass, including auth-switch from `caching_sha2_password` |
| Text protocol (`COM_QUERY`) | Pass |
| Prepared statements (`COM_STMT_PREPARE` / `EXECUTE`) | Pass, including statement reuse |
| Binary parameter binding | Pass — int, string, NULL |
| Transactions (`BEGIN` / `COMMIT`) | Pass |
| TCP listener | Pass |
| Unix socket listener | Pass |
| Prisma 5.4.2 client | Connects, queries, transacts, disconnects cleanly |
| `prisma db push` | Runs to completion |
| mysql2 (independent JS driver) | Pass |

**Cold start, measured over 25 runs:** spawn to accepting connections **1.85ms median**
(p95 2.10ms); spawn to a client holding the handshake **1.99ms median**. Release binary is
636K. Against the 100ms per-test-file budget this leaves roughly 98ms for state image load.

## What Prisma actually requires

### The connection preamble is load-bearing

Every Prisma connection opens with:

```sql
SELECT @@socket, @@max_allowed_packet, @@wait_timeout
```

If this returns zero rows, `mysql_async` — the driver inside Prisma's query engine —
calls `Option::unwrap()` on the missing row and **panics**. It does not return an error.
A MySQL-compatible server has no choice about implementing system variables; they are the
first thing on the wire.

The migration engine additionally reads `@@version`, `@@GLOBAL.version`, and
`@@lower_case_table_names`, and writes one of them as `SELECT @@GLOBAL.version version` —
an alias by juxtaposition with no `AS`, which a naive parser misses.

### `prisma migrate` is an information_schema client

Before emitting any DDL, the migration engine runs nine introspection queries. Every one
targets `information_schema`:

| Table | Purpose |
|---|---|
| `VIEWS` | view definitions |
| `TABLES` + `COLUMNS` | table inventory, create options, comments |
| `TABLE_CONSTRAINTS` + `CHECK_CONSTRAINTS` | check constraints |
| `COLUMNS` | full column metadata, ordered by `ordinal_position` |
| `KEY_COLUMN_USAGE` + `REFERENTIAL_CONSTRAINTS` | foreign keys with delete/update rules |
| `STATISTICS` | indexes, including `sub_part` and `seq_in_index` |
| `ROUTINES` | stored procedures |

These are not simple lookups. They use the `BINARY` operator for case-sensitive comparison
(in select lists, join conditions, `WHERE`, and `ORDER BY`), `LOWER()`, `IF()`, `DISTINCT`,
and multi-table joins across information_schema itself.

**Consequence for the roadmap:** `information_schema` is not a late-stage nicety. It is a
prerequisite for `prisma migrate deploy`, which is the first milestone of Phase 8, and it
must be queryable as ordinary tables by the real executor rather than special-cased.

### DDL shape

The DDL Prisma emits for an ordinary model:

```sql
CREATE TABLE `Widget` (
    `id` VARCHAR(21) NOT NULL,
    `name` VARCHAR(120) NOT NULL,
    `count` INTEGER NOT NULL DEFAULT 0,
    `isDeleted` BOOLEAN NOT NULL DEFAULT false,
    `createdAt` DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (`id`)
) DEFAULT CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci
```

Note the fractional-second precision on both `DATETIME(3)` and `CURRENT_TIMESTAMP(3)`, and
that charset and collation are specified per table.

## Caveats

`prisma db push` reporting success is **not** a compatibility result. The spike handler
returns an empty result set for anything it does not recognise, so the migration engine
concluded the database was empty and issued its `CREATE TABLE`, which was accepted and
discarded. The value here is the captured statement sequence, not the exit code.

## Bugs this surfaced

Two real defects, both fixed:

- **Unix socket paths over ~100 bytes fail** with a bare `SUN_LEN` OS error naming neither
  the limit nor the path. Harnesses that build socket paths under a deep temp directory hit
  this immediately. Now checked with an explicit message.
- **Aliases by juxtaposition** (`SELECT @@GLOBAL.version version`) were not parsed, only the
  `AS` form. Prisma's migration engine uses the bare form.

## Review hardening

A high-effort code review over the spike output found eleven issues, all fixed before the
code landed, each with a regression test:

- **A client could panic the server.** `days * 24` in the binary TIME decoder overflows for
  any `days` above 178_956_970, a value a client simply sends. With `panic = "abort"` in the
  release profile that takes the process down. Now computed in `u64` and range-checked.
- **Silently wrong parameters on re-execution.** A client may omit parameter types when
  re-executing a prepared statement, expecting the server to remember them. Defaulting to
  VAR_STRING read a raw 8-byte LONGLONG as a length-encoded string, corrupting that value
  and misaligning every parameter after it. Types are now remembered per statement, and a
  re-execution with neither types nor history is a clear error.
- **Binary width came from the value, not the column.** A handler declaring `ColumnType::Long`
  and returning a `Value::I64` emitted 8 bytes where the client reads 4, desynchronising the
  result set. Encoding now follows the declared type.
- **Unvalidated length bytes** in the DATETIME and TIME decoders let a malformed field consume
  bytes belonging to the next one.
- **Unbounded memory** via `COM_STMT_SEND_LONG_DATA`, including under statement ids that were
  never prepared and so were never freed. Now capped and tied to real statements.
- **Multi-statement capabilities were advertised but not implemented**, so a client enabling
  them would silently lose every result after the first. Dropped from the advertised set until
  the multi-result terminator exists.
- Plus: unchecked row width against the column list, two sources of truth for the statement id,
  `--socket` unlinking whatever file it was pointed at, no `PROTOCOL_41` validation, and a CI
  gate that could pass green with its supply-chain checks skipped.

The theme is worth keeping in view: nearly all of these produce **wrong answers rather than
errors**, which is the one failure mode a test database must never have. A test suite that
trusts this engine has no way to notice a silently corrupted parameter.

## Reproducing

```sh
cargo build --release
./target/release/mysql-ultralight --port 43306 --trace
# then point any MySQL client at 127.0.0.1:43306 with any credentials
```

`--trace` logs every connection, capability set, command, and result. That is how the
findings above were gathered, and it is the seed of the query-log capability in Phase 9.
