# mysql-ultralight

A fast-starting, MySQL 8 compatible storage engine for tests.

Production runs MySQL 8. Testing against it is slow: containers take seconds to become
healthy and reset costs a meaningful slice of every test file. `mysql-ultralight` aims to
be a drop-in replacement for that role — same wire protocol, same SQL, same error codes —
that starts in milliseconds and holds everything in memory.

It is **not** for production. There is no durability, and there never will be.

## Status

It runs a real application's database. All **604 migrations** of a production Prisma
history apply — re-checked 2026-09-08, and a second `migrate deploy` against the result
answers "No pending migrations to apply" — `prisma db push` creates a schema, and Prisma
Client does CRUD against it. (604 is what `prisma migrate deploy` reports for that
directory; a plain `ls` says 605, because one entry is `migration_lock.toml`.)

It is also checked against MySQL itself, not only against its own tests: **5,608
statements** of a differential corpus are put to a real `mysql:8.3` and to this engine over
both the text and binary protocols, and the answers are compared. **0 diverge.** Every
divergence found and not yet fixed is a filed issue rather than an accepted baseline —
`crates/differential/baseline.txt` holds no entries, and may only shrink.

| | Real MySQL 8 | mysql-ultralight |
|---|---|---|
| Start | 4.5–8.9s of container | **0.1ms** |
| Schema | 26.1s of `prisma migrate deploy` | **1.3–1.7ms** from a baked image |

The second row is the point. Bake the migrations once into a state image, and every test
process loads the whole schema — 278 tables, 2,771 columns, seed rows and all their
constraints — in about a millisecond. `prisma migrate deploy` against a server booted from
one answers "No pending migrations to apply".

```sh
mysql-ultralight --port 3306 &                       # bake, once
npx prisma migrate deploy
mysql -e "SELECT ultralight_snapshot('schema.mus')"

mysql-ultralight --state schema.mus                  # every test process
```

See [`docs/prototype.md`](docs/prototype.md) for what works and what does not,
[`docs/baseline-mysql.md`](docs/baseline-mysql.md) for the numbers it has to beat, and
[`docs/conformance-surface.md`](docs/conformance-surface.md) for the SQL surface a real
codebase demands.

Work is tracked in [motte](https://codevachon.github.io/motte) — run `motte ready` to see
what is actionable.

## Try it

```sh
cargo build --release
./target/release/mysql-ultralight --port 3307 --trace
```

Then point any MySQL client at `127.0.0.1:3307` with any credentials:

```
mysql://test:test@127.0.0.1:3307/testdb
```

`--trace` logs every connection, negotiated capability, command, and result to stderr,
which is the fastest way to see what a client actually asks a MySQL server for.

### Options

| Flag | Meaning |
|---|---|
| `--port <PORT>` | TCP port; `0` picks a free one (default) |
| `--socket <PATH>` | Also listen on a unix socket. Keep the path under ~100 bytes |
| `--database <NAME>` | Database created at startup (default: `test`) |
| `--password <PASS>` | Require this password; omitted means any credentials work |
| `--state <PATH>` | Load a baked state image instead of an empty catalog |
| `--server-version <V>` | Version string reported to clients |
| `--trace` | Log all protocol activity to stderr |

On startup the server prints one line to stdout so a harness can parse the port and know
it is ready without polling. The instance id is fresh per process and is served back as
`@@ultralight_instance`, so a harness can tell the server it started from a leftover one
holding the same port:

```
ready port=3307 socket=- startup_ms=1.312 instance=3f1c0a9b6d2e4517
```

## Layout

| Crate | Responsibility |
|---|---|
| `crates/protocol` | Wire codec: framing, capabilities, auth, handshake, columns, values |
| `crates/server` | Connection state machine, command dispatch, tracing |
| `crates/engine` | Catalog, storage, expressions, executor, sessions, `information_schema`, the state image |
| `crates/cli` | The `mysql-ultralight` binary, and the engine/server adapter |
| `crates/conformance` | Extracts a target codebase's SQL surface |
| `crates/differential` | Runs the same SQL against MySQL and this engine and diffs the answers |
| `packages/npm` | `@codevachon/mysql-ultralight`: the JS API and platform packages |
| `examples/app` | The example application: one schema, one history, the controllers |
| `examples/node-test` | Testing it with `node:test`, and the engine-level claims |
| `examples/jest` | Testing it with Jest, shared and per-file |
| `examples/vitest` | Testing it with Vitest, the same two ways |

## From JavaScript

```sh
npm install --save-dev @codevachon/mysql-ultralight
```

```js
import { start } from '@codevachon/mysql-ultralight';

const db = await start({ image: '.cache/schema.mus' });
process.env.DATABASE_URL = db.url;
// ...
await db.stop();
```

Port 0 by default, so parallel test workers cannot race for one, and `start()` resolves
only once the server is accepting connections. See
[`packages/npm/README.md`](packages/npm/README.md).

[`examples/app`](examples/app) is the worked version of all of this: a real schema, a real
migration history, and the controllers a test suite would exercise.
[`node-test`](examples/node-test), [`jest`](examples/jest) and
[`vitest`](examples/vitest) contain nothing but tests, and all three test that same app —
comparing runners is only useful if the thing under test is identical.

Each runner example shows two models: the **drop-in**, one database for the whole run,
which is the smallest change to a suite that already talks to MySQL; and **per-file
isolation**, a fresh database for every test file, which is what a millisecond startup
makes affordable and a real MySQL does not. Both prove the difference rather than
asserting it — two test files claim the same unique value, which collides under a shared
database and does not under isolation.

They run as part of the gate, so the documented path is the tested one.

## Development

```sh
./scripts/check.sh          # the gate: fmt, clippy, tests, npm tests, the Prisma
                            # example, unused deps, advisories, and Fallow over
                            # the npm package
./scripts/check.sh --fix    # format and apply clippy's suggestions first
./scripts/check.sh --fast   # skip the slower supply-chain checks
```

### Checking this engine against MySQL rather than against memory

The gate checks the engine against its own tests. Three scripts check it against a real
`mysql:8.3` in Docker, which is a different question and the one that has been wrong more
often.

```sh
./scripts/differential.sh   # the corpus: same SQL to both servers, diff the answers
./scripts/ask.sh q.sql      # one question, right now, both servers, side by side
./scripts/mutate.sh ...     # remove the code a test covers and confirm the test fails
```

`differential.sh` is deliberately **not** in the gate — it needs a MySQL to compare
against, and a checkout cannot be assumed to have one. Run it when changing anything the
engine answers with. It is the only thing here that compares this engine to MySQL rather
than to what somebody remembered about MySQL. See
[`crates/differential`](crates/differential/README.md).

`ask.sh` is for settling one question before working it: an issue body is prose somebody
wrote once, and in this project the prose has been wrong often enough that measuring first
is cheaper than implementing a premise twice. `COLINFO=1` compares column metadata too,
because a claim about a type, collation or flag is invisible in the rows.

`mutate.sh` is for the other failure: a test that passes without the code it covers.
Several here did — assertions that never ran, a test reading a struct field where the bug
was in the wire flag, sample values that sorted the same way whether the comparison was
right or wrong.

The examples need their dependencies once — `for d in examples/*/; do npm install --prefix
"$d"; done`. Until then the gate reports them as skips rather than passing over them.

A pre-commit hook runs the same script. A fresh clone needs `git config core.hooksPath
.githooks` to enable it.

## License

MIT OR Apache-2.0
