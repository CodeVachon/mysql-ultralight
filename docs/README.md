# mysql-ultralight

A fast-starting, MySQL 8 compatible storage engine for tests. Rust, in memory, no
durability. Not for production, ever.

This site is the design and measurement record: what was measured before the engine
was built, what it was decided to be, and what it can currently do. Installing it and
using it from a test suite is covered by the
[README](https://github.com/CodeVachon/mysql-ultralight#readme) and the
[npm package](https://github.com/CodeVachon/mysql-ultralight/tree/main/packages/npm).

Numbers live on the page that measured them, with the date they were taken. Nothing is
restated here, because a number in two places is a number that disagrees with itself
later.

## Design

- [**ADR 001 — Engine architecture**](adr-001-architecture.md) — the accepted shape of
  the engine, and why each alternative was rejected.
- [**Track C — Rust wire protocol viability**](spike-wire-protocol.md) — whether a
  hand-written MySQL server could reach a real client at all, answered by reaching one.

## Measurements

- [**Track A — What real MySQL costs**](baseline-mysql.md) — container start and
  migration time for the production stack, which is the bar this has to beat.
- [**The working prototype**](prototype.md) — what works end to end and what does not,
  kept current, with each capability marked by how it was checked.
- [**Conformance surface**](conformance-surface.md) — the SQL a real 203-model codebase
  actually issues, extracted from it rather than guessed at.
