# ADR 001 — Engine architecture

Status: accepted, 2026-08-18. Supersedes the open questions in Phase 0.

## Context

Phase 0 measured three things. Real MySQL costs 26.1s of `prisma migrate deploy` before
the first assertion of every test run, on top of 4.5–8.9s of container start in CI. A
hand-written Rust MySQL server reaches Prisma's client in 1.85ms. And sqlparser-rs parses
96.3% of the 4,661 SQL statements this codebase actually contains, with the remainder
falling into five named gaps.

## Decisions

**Rust, from scratch, on sqlparser-rs.** The 96.3% figure settles it: the parser is a
solved problem to within five extensions, and everything above it — catalog, storage,
executor — is where the value is. Track B (adopting go-mysql-server) was cancelled.

**Crate boundaries follow the protocol/SQL split.**

| Crate | Owns | Knows nothing about |
|---|---|---|
| `protocol` | Wire codec, auth, framing | SQL, storage |
| `server` | Connections, command dispatch | SQL, storage |
| `engine` | Catalog, storage, executor, sessions | Sockets, packets |
| `cli` | Process lifecycle, listeners | — |
| `conformance` | Target-codebase analysis | Runtime |

`server` reaches the engine through the `Handler` trait it already defines. That seam
existed before the engine did, and keeping it means the protocol layer stays testable
without a database and the engine stays testable without a socket.

**One handler per connection, synchronous.** The engine is in-memory with no IO in the
query path. Async would add a boxed future per statement and buy nothing. Shared state
lives behind an `Arc<RwLock<..>>`; session state is owned outright by the handler.

**Wrong answers are worse than errors.** Every review of this codebase so far has found
defects of the same shape: a plausible value returned where an error belonged. A test
suite trusting this engine cannot detect that. Unimplemented constructs therefore return
`ErrPacket::unsupported`, never a guess.

**`sql_mode` is a parser input, not a runtime flag.** The target sets `ANSI_QUOTES` and
`PIPES_AS_CONCAT`, which change what a query *means*. This cannot be retrofitted, so the
mode is threaded from configuration into parsing from the start.

**`information_schema` is a real, queryable schema.** Prisma's migration engine issues
nine introspection queries against it before emitting any DDL, joining catalog tables to
each other. It is served by generating rows from the catalog on read, so ordinary query
execution applies rather than a special case.

## Build order

Driven by what blocks what, not by layering aesthetics.

1. **System variables and the connection preamble** (#0017). The first thing on the wire.
   Returning no row makes Prisma's driver panic rather than error.
2. **Catalog, storage, and DDL.** `CREATE TABLE` / `ALTER TABLE` / `DROP`, typed columns,
   constraints recorded even where not yet enforced.
3. **Expressions and DML.** `INSERT` / `UPDATE` / `DELETE` / `SELECT` with `WHERE`,
   `ORDER BY`, `LIMIT`, and bound parameters — the shape Prisma Client emits.
4. **`information_schema`** (#0016), unblocking `prisma migrate deploy`.
5. **The five parser gaps** (#0018, #0020, #0021), of which user variables and
   `PREPARE`/`EXECUTE` are 162 of the 173 rejected statements.
6. **State image** (#0007), the thing that converts a 26s setup into a sub-100ms one.

## What is deliberately not decided yet

Index structures, the on-wire state image format, and the transaction model. Each wants
evidence the prototype has not yet produced. Recording them as open beats guessing now
and calling it architecture.
