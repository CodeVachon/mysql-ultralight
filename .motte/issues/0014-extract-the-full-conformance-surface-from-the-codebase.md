---
id: 14
title: Extract the full conformance surface from the codebase
state: Todo
parent: 1
labels: [spike, analysis]
created: 2026-08-18T14:11:21Z
updated: 2026-08-18T14:11:21Z
---

## Description

Turn the target codebase into a machine-readable feature checklist that scopes every later phase.

Sources: 5673-line schema.prisma (203 models, 66 enums), 605 migrations (2.9MB SQL), 419 raw-SQL call sites across 149 files.

Already established: no views, triggers, stored procedures, fulltext, spatial indexes or partitioning. Present and required: generated columns (3), CHECK constraints (3), foreign keys (735) with CASCADE and SET NULL, COLLATE (298), window functions, CTEs including one RECURSIVE, GROUP_CONCAT, JSON functions, ST_Distance_Sphere and POINT, ON DUPLICATE KEY UPDATE, multi-table UPDATE with JOIN.

Deliverable: a checklist file the conformance harness can consume, so 'done' is countable rather than a judgement call.
