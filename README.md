# mysql-ultralight

A fast-starting, MySQL 8 compatible storage engine for tests.

Production runs MySQL 8. Testing against it is slow: containers take seconds to become
healthy and reset costs a meaningful slice of every test file. `mysql-ultralight` aims to
be a drop-in replacement for that role — same wire protocol, same SQL, same error codes —
that starts in milliseconds and holds everything in memory.

It is **not** for production. There is no durability, and there never will be.

## Status

It runs a real application's database. All **604 migrations** of a production Prisma
history apply, `prisma db push` creates a schema, and Prisma Client does CRUD against it.

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
it is ready without polling:

```
ready port=3307 socket=- startup_ms=1.312
```

## Layout

| Crate | Responsibility |
|---|---|
| `crates/protocol` | Wire codec: framing, capabilities, auth, handshake, columns, values |
| `crates/server` | Connection state machine, command dispatch, tracing |
| `crates/engine` | Catalog, storage, expressions, executor, sessions, `information_schema` |
| `crates/cli` | The `mysql-ultralight` binary, and the engine/server adapter |
| `crates/conformance` | Extracts a target codebase's SQL surface |
| `packages/npm` | `@codevachon/mysql-ultralight`: the JS API and platform packages |
| `examples/prisma-app` | A worked Prisma application, run by the gate |

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

[`examples/prisma-app`](examples/prisma-app) is the worked version of all of this: a real
schema, a real migration history, and tests that bake an image and run Prisma Client
against it. It runs as part of the gate, so the documented path is the tested one.

## Development

```sh
./scripts/check.sh          # the gate: fmt, clippy, tests, npm tests, the Prisma
                            # example, unused deps, advisories, and Fallow over
                            # the npm package
./scripts/check.sh --fix    # format and apply clippy's suggestions first
./scripts/check.sh --fast   # skip the slower supply-chain checks
```

The example needs its dependencies once: `(cd examples/prisma-app && npm install)`. Until
then the gate reports it as a skip rather than passing over it.

A pre-commit hook runs the same script. A fresh clone needs `git config core.hooksPath
.githooks` to enable it.

## License

MIT OR Apache-2.0
