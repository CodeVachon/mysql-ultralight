---
id: 5
title: Phase 4 — Storage engine
state: Done
labels: [epic, phase-4]
blockedBy: [4]
created: 2026-08-18T14:10:54Z
updated: 2026-08-19T12:48:15Z
---

## Description

In-memory storage: tables, rows, secondary indexes, primary key and unique constraint enforcement, foreign keys with ON DELETE CASCADE / SET NULL and ON UPDATE CASCADE (the migrations use all three heavily), transactions with savepoints, and generated column evaluation.

No durability requirement — data is discarded at process exit. The design constraint that matters is fast load from a pre-baked state image, not fast persistence.

## Notes

### 2026-08-19T12:48:15Z — Christopher Vachon (user)

Closed 2026-08-19. The catalog is the storage engine: databases, tables, columns, rows, indexes, foreign keys, checks and auto-increment counters, with case-insensitive lookup, snapshot-based transactions, and a binary state image that round trips all of it. Everything is in memory and nothing is durable, which is the design.
