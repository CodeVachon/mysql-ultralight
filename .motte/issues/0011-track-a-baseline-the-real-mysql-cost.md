---
id: 11
title: Track A — Baseline the real MySQL cost
state: Todo
parent: 1
labels: [spike, benchmark]
created: 2026-08-18T14:11:21Z
updated: 2026-08-18T14:11:21Z
---

## Description

Measure the status quo so every later claim of 'faster' has a number behind it.

Target: ~/Repositories/Appello/application-mono-repo (mysql:8.3, Prisma 5.4.2, jest).

Measure and record:
- docker compose up to a connection-ready mysql:8.3
- prisma migrate deploy across all 605 migrations, wall clock
- per-test-file cost in the existing jest suite: setup, teardown, DB reset
- peak memory of the MySQL container

Deliverable: a numbers table committed to the repo as the benchmark baseline.
