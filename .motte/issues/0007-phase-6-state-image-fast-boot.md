---
id: 7
title: Phase 6 — State image (fast boot)
state: Todo
labels: [epic, phase-6]
blockedBy: [5]
created: 2026-08-18T14:10:54Z
updated: 2026-08-18T14:11:32Z
---

## Description

The mechanism that makes fresh-process-per-test-file viable. Replaying 605 migrations per test process costs seconds; loading a pre-baked image must cost milliseconds.

Build: a binary state image format capturing full schema plus seed rows, a bake step that runs migrations once and caches the result keyed by migration hash, and a boot path that loads the image (mmap-friendly) rather than executing SQL. Seed data support is a requirement, not an extra.

Target: spawn plus load under 100ms.
