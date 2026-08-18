---
id: 9
title: Phase 8 — Conformance harness
state: Todo
labels: [epic, phase-8]
blockedBy: [3, 6]
created: 2026-08-18T14:10:54Z
updated: 2026-08-18T14:41:18Z
---

## Description

The definition of done. v1 ships when the real Appello schema works against this engine.

Build a differential test harness that runs identical SQL against mysql:8.3 and against mysql-ultralight and compares results, error codes, and types. Feed it the 605 migrations, the 419 raw-SQL call sites, and Prisma's generated queries for the 203 models and 66 enums.

Milestone ladder: (1) prisma migrate deploy completes, (2) Prisma client connects and round-trips every model, (3) the existing jest suite passes.

## Notes

### 2026-08-18T14:41:18Z — Christopher Vachon (user)

Track C established the ordering for the Phase 8 milestone ladder: information_schema support (see the Phase 4 child issue) gates prisma migrate deploy, because the migration engine introspects before it emits DDL. Phase 8 cannot start its first milestone until that lands.
