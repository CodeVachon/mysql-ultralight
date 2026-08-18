# mysql-ultralight

A fast-starting, MySQL 8 compatible storage engine for tests.

Production runs MySQL 8. Testing against it is slow: containers take seconds to become
healthy and reset costs a meaningful slice of every test file. `mysql-ultralight` aims to
be a drop-in replacement for that role — same wire protocol, same SQL, same error codes —
that starts in milliseconds and holds everything in memory.

It is **not** for production. There is no durability, and there never will be.

## Status

Working prototype. `prisma db push` creates a schema against it, and Prisma Client does
CRUD — create, findMany, findUnique, filtering, count, update, pagination, delete.

**Cold start is 2.05ms median** from process spawn to a client holding the handshake. The
equivalent on real MySQL is 4.5–8.9s of container start plus 26.1s of `prisma migrate
deploy`, measured on the same machine against the same schema.

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

## Development

```sh
./scripts/check.sh          # the gate: fmt, clippy, tests, unused deps, advisories
./scripts/check.sh --fix    # format and apply clippy's suggestions first
```

A pre-commit hook runs the same script. A fresh clone needs `git config core.hooksPath
.githooks` to enable it.

## License

MIT OR Apache-2.0
