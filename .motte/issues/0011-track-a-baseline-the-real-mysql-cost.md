---
id: 11
title: Track A — Baseline the real MySQL cost
state: Done
parent: 1
labels: [spike, benchmark]
created: 2026-08-18T14:11:21Z
updated: 2026-08-18T15:37:17Z
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

## Notes

### 2026-08-18T15:37:17Z — Christopher Vachon (user)

Measured 2026-08-18 on mysql:8.3 with the repo's my.cnf and flags, in isolated containers. Empty container to first query 4.5-5.4s; with the 18MB dumps 8.9s; warm restart 0.52s; prisma migrate deploy over 604 migrations 26.1s; empty to fully migrated 30.6s; 392-519MB idle. Resulting schema 277 tables, 581 foreign keys, 1015 indexes.

The dominant cost is not the container. jest globalSetup creates a TEST<epoch> schema on the already-running MySQL and runs prisma migrate deploy, so every test run pays about 26 seconds before the first assertion. A sub-100ms state image boot turns that into roughly 0.1s, a 260x improvement on the number developers actually wait for. Full detail in docs/baseline-mysql.md.
