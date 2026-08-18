---
id: 14
title: Extract the full conformance surface from the codebase
state: Todo
parent: 1
labels: [spike, analysis]
created: 2026-08-18T14:11:21Z
updated: 2026-08-18T15:37:17Z
---

## Description

Turn the target codebase into a machine-readable feature checklist that scopes every later phase.

Sources: 5673-line schema.prisma (203 models, 66 enums), 605 migrations (2.9MB SQL), 419 raw-SQL call sites across 149 files.

Already established: no views, triggers, stored procedures, fulltext, spatial indexes or partitioning. Present and required: generated columns (3), CHECK constraints (3), foreign keys (735) with CASCADE and SET NULL, COLLATE (298), window functions, CTEs including one RECURSIVE, GROUP_CONCAT, JSON functions, ST_Distance_Sphere and POINT, ON DUPLICATE KEY UPDATE, multi-table UPDATE with JOIN.

Deliverable: a checklist file the conformance harness can consume, so 'done' is countable rather than a judgement call.

## Notes

### 2026-08-18T15:29:25Z — Christopher Vachon (user)

Correction on 2026-08-18: the claim that views, triggers and stored procedures are absent was drawn from the Prisma migrations directory alone and is wrong for views. provisions/mysql/3-anzen-views.sql defines 8 views that are loaded via docker-entrypoint-initdb.d. Any conformance checklist this issue produces must cover provisions/ and the dump files, not only migrations/. Triggers and stored procedures remain unobserved but should be re-checked against the same sources.

### 2026-08-18T15:37:17Z — Christopher Vachon (user)

Method requirement established by Track A: two of three grep-based passes over the migrations returned wrong answers, because SQL statements wrap across lines and because provisions/ was never searched. The checklist must come from parsed or whitespace-normalised SQL and must cover migrations/, provisions/, and the app's raw queries.
