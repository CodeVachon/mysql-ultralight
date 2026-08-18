---
id: 14
title: Extract the full conformance surface from the codebase
state: Done
parent: 1
labels: [spike, analysis]
created: 2026-08-18T14:11:21Z
updated: 2026-08-18T16:06:13Z
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

### 2026-08-18T15:51:58Z — Christopher Vachon (user)

Completed 2026-08-18. Built crates/conformance, a tool that extracts SQL from Prisma migrations, the provisioning dumps and TypeScript raw-query templates, then feeds every statement to sqlparser-rs with the MySQL dialect.

Result: 4,661 statements across 773 files, 96.3 percent parsed by sqlparser-rs unmodified (migrations 94.6, provisions 100, application code 99.0 with 474 Prisma.sql fragments excluded as not being standalone statements). The 173 rejections form only 6 distinct gaps, and 162 of them are user variables with := and PREPARE/EXECUTE dynamic SQL.

Inventory: 14 statement kinds, 33 constructs, 14 data types, 74 functions. Machine-readable output in docs/conformance-surface.json for the Phase 8 harness, human-readable in docs/conformance-surface.md. Both regenerate with cargo run -p mysql-ultralight-conformance.

### 2026-08-18T16:06:13Z — Christopher Vachon (user)

Post-review hardening: /code-review at high effort found 12 issues in the extractor, all fixed with regression tests (28 tests in the crate). Three were high severity and each had produced wrong output: template escape sequences were dropped along with the character they escaped, so SELECT a\\nFROM t became SELECT aFROM t and was reported as a parser gap that does not exist; application-code line numbers pointed into a synthetic concatenation rather than the real file; and single-word statements such as COMMIT were classified as fragments and never parsed at all.

Also corrected a reporting honesty problem the review caught: 193 application-code statements were being removed from the parse-rate denominator by a broad match on the error text, without that being visible. The reclassification is now restricted to application code, where a placeholder in identifier position really is an artifact of static extraction, and both the fragment and unrecoverable counts are columns in the report. The headline rate is unchanged at 96.3 percent but is now auditable.
