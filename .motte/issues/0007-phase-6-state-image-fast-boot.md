---
id: 7
title: Phase 6 — State image (fast boot)
state: Todo
labels: [epic, phase-6]
blockedBy: [5]
created: 2026-08-18T14:10:54Z
updated: 2026-08-18T15:37:17Z
---

## Description

The mechanism that makes fresh-process-per-test-file viable. Replaying 605 migrations per test process costs seconds; loading a pre-baked image must cost milliseconds.

Build: a binary state image format capturing full schema plus seed rows, a bake step that runs migrations once and caches the result keyed by migration hash, and a boot path that loads the image (mmap-friendly) rather than executing SQL. Seed data support is a requirement, not an extra.

Target: spawn plus load under 100ms.

## Notes

### 2026-08-18T15:37:17Z — Christopher Vachon (user)

Track A quantified the target: the setup this replaces costs 26.1s per test run (prisma migrate deploy over 604 migrations in jest globalSetup), plus 4.5-8.9s of container start in CI. Sub-100ms image boot is therefore worth roughly 260x on the developer-visible wait, which is the justification for the state image format.
