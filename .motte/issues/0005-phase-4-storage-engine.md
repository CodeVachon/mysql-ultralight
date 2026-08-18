---
id: 5
title: Phase 4 — Storage engine
state: Todo
labels: [epic, phase-4]
blockedBy: [4]
created: 2026-08-18T14:10:54Z
updated: 2026-08-18T14:11:32Z
---

## Description

In-memory storage: tables, rows, secondary indexes, primary key and unique constraint enforcement, foreign keys with ON DELETE CASCADE / SET NULL and ON UPDATE CASCADE (the migrations use all three heavily), transactions with savepoints, and generated column evaluation.

No durability requirement — data is discarded at process exit. The design constraint that matters is fast load from a pre-baked state image, not fast persistence.
