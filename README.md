# mysql-ultralight

A fast-starting, MySQL 8 compatible storage engine for tests.

Production runs MySQL 8. Testing against it is slow: containers take seconds to become
healthy and reset costs a meaningful slice of every test file. `mysql-ultralight` aims to
be a drop-in replacement for that role — same wire protocol, same SQL, same error codes —
that starts in milliseconds and holds everything in memory.

It is **not** for production. There is no durability, and there never will be.

## Status

Early. The wire protocol works; the SQL engine does not exist yet.

Prisma 5.4.2 and mysql2 both connect, run queries, use prepared statements with bound
parameters, and transact. Cold start is **1.85ms median** from process spawn to accepting
connections. See [`docs/spike-wire-protocol.md`](docs/spike-wire-protocol.md) for what that
proved and what it revealed about the road ahead.

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
| `crates/cli` | The `mysql-ultralight` binary |

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all
```

## License

MIT OR Apache-2.0
