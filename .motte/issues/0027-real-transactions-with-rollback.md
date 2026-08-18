---
id: 27
title: Real transactions with rollback
state: Done
parent: 6
labels: [executor]
created: 2026-08-18T19:51:39Z
updated: 2026-08-18T20:01:39Z
---

## Description

BEGIN and COMMIT are accepted and nothing rolls back. That is safe only because the engine discards state at process exit; within a process a failed statement leaves partial writes behind, and any test that relies on rollback for isolation silently gets none.

Scope for an in-memory test engine: snapshot the catalog on BEGIN, restore it on ROLLBACK, discard the snapshot on COMMIT, with SAVEPOINT and ROLLBACK TO SAVEPOINT as named snapshots. That gives correct single-connection semantics, which is what a test suite needs.

It does not give isolation between concurrent connections; the catalog is shared and a second connection sees uncommitted writes. That limitation should be documented rather than implied away, and revisited if the fresh-process-per-test-file model ever changes.

## Notes

### 2026-08-18T20:01:39Z — Christopher Vachon (user)

Implemented 2026-08-18. BEGIN snapshots the catalog, ROLLBACK restores it, COMMIT discards it, and SAVEPOINT / ROLLBACK TO SAVEPOINT / RELEASE SAVEPOINT work as named snapshots. Schema changes are undone too, since the whole catalog is snapshotted; MySQL commits implicitly on DDL instead, so this is more forgiving rather than less.

Two limitations are documented rather than implied away. There is no isolation between concurrent connections: the catalog is shared, so a second connection sees uncommitted writes and a rollback discards them. And the snapshot is a whole-catalog copy rather than an undo log, which is the honest trade for an in-memory engine holding a test-sized dataset. Both are correct enough for the fresh-process-per-test-file model and should be revisited if that changes.
