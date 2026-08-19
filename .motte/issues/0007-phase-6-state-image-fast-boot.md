---
id: 7
title: Phase 6 — State image (fast boot)
state: Done
labels: [epic, phase-6]
blockedBy: [5]
created: 2026-08-18T14:10:54Z
updated: 2026-08-19T12:48:15Z
---

## Description

The mechanism that makes fresh-process-per-test-file viable. Replaying 605 migrations per test process costs seconds; loading a pre-baked image must cost milliseconds.

Build: a binary state image format capturing full schema plus seed rows, a bake step that runs migrations once and caches the result keyed by migration hash, and a boot path that loads the image (mmap-friendly) rather than executing SQL. Seed data support is a requirement, not an extra.

Target: spawn plus load under 100ms.

## Notes

### 2026-08-18T15:37:17Z — Christopher Vachon (user)

Track A quantified the target: the setup this replaces costs 26.1s per test run (prisma migrate deploy over 604 migrations in jest globalSetup), plus 4.5-8.9s of container start in CI. Sub-100ms image boot is therefore worth roughly 260x on the developer-visible wait, which is the justification for the state image format.

### 2026-08-19T01:58:50Z — Christopher Vachon (user)

Built 2026-08-18. A hand-rolled binary image of the whole catalog, written with SELECT ultralight_snapshot('path') and loaded with --state, plus ultralight_restore for resetting a running server between test files.

Measured against the real history: prisma migrate deploy costs 26.1s and produces a 630 KB image in 29ms; booting from that image takes 0.9-1.7ms, with 241 tables, 2367 columns, seed rows and every constraint intact. A server booted from the image is indistinguishable from one that ran the migrations, and prisma migrate deploy against it reports them already applied.

Baking it also found a protocol bug nothing else had: COM_STMT_PREPARE declared zero columns, so clients that cache prepare metadata lost every column name and read results positionally. That is why Prisma could not find _prisma_migrations in an introspection result that contained it.
