---
id: 9
title: Phase 8 — Conformance harness
state: Todo
labels: [epic, phase-8]
blockedBy: [3, 6]
created: 2026-08-18T14:10:54Z
updated: 2026-08-20T14:36:42Z
---

## Description

The definition of done. v1 ships when the real Appello schema works against this engine.

Build a differential test harness that runs identical SQL against mysql:8.3 and against mysql-ultralight and compares results, error codes, and types. Feed it the 605 migrations, the 419 raw-SQL call sites, and Prisma's generated queries for the 203 models and 66 enums.

Milestone ladder: (1) prisma migrate deploy completes, (2) Prisma client connects and round-trips every model, (3) the existing jest suite passes.

## Notes

### 2026-08-18T14:41:18Z — Christopher Vachon (user)

Track C established the ordering for the Phase 8 milestone ladder: information_schema support (see the Phase 4 child issue) gates prisma migrate deploy, because the migration engine introspects before it emits DDL. Phase 8 cannot start its first milestone until that lands.

### 2026-08-20T14:36:42Z — Christopher Vachon (user)

The harness exists. crates/differential runs identical SQL against a real mysql:8.3 and against mysql-ultralight with one client, and diffs rows, column names, column types, nullability and error codes. scripts/differential.sh starts MySQL in Docker, starts the engine, compares, and tears down - 7 seconds cold, and deliberately not in scripts/check.sh, because a checkout cannot be assumed to have Docker.

The corpus leads with a group called 'answers I got wrong': every statement in it is one this engine once answered incorrectly. Those were all caught by a reviewer who happened to know MySQL, which does not scale and did not always work - JSON_QUOTE was changed to return a document on an argument that sounded right, and MySQL returns a string.

baseline.txt records what is already known, and two things fail a run: a divergence not in the baseline, and a baseline line that no longer differs. So the file can only shrink, and it cannot quietly describe a state that has passed.

First run: 108 divergences across 66 statements, in eight classes, filed as #0036 to #0043. The wrong answers rather than wrong metadata are #0036 (UPPER, TRIM, SUBSTRING, LEFT, RIGHT return '' for NULL and LENGTH returns 0, where MySQL returns NULL) and #0037 (JSON rendered compactly where MySQL spaces it). #0038 and #0039 are the two large metadata classes: every integer column described as BIGINT, and a NOT NULL string column described as nullable. #0041 also corrects a test comment written earlier this week claiming MySQL reports 0 for a DATE's DATETIME_PRECISION - it reports NULL.

Review found eight problems in the harness itself, all fixed. The ones worth remembering: the summary underflowed to 18446744073709551615 with no baseline; a baseline key was shared by two divergences of the same kind in one statement, so a half-fix read as no change - the same hole the duplicate-statement guard had been added to close; a column-name mismatch skipped that column's type, so fixing a name would surface hidden divergences and fail the run for getting better; a failed setup neither failed the run nor blocked --bless, and a setup that fails on both servers makes every case compare equal, reporting a group that tested nothing as a group that agreed; and the mysql crate's 'minimal' feature resolves to flate2/zlib, so it pulled libz-sys and put a C toolchain in the way of cargo test --workspace - 'minimal-rust' gives the same client with no native code.

Verified by breaking month clamping in the engine and watching the harness report it as new with exit 1, then go green when restored.

Still to do for this epic: feed it the 604 migrations, the raw-SQL call sites, and Prisma's generated queries.
