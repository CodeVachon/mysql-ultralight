---
id: 12
title: Track B — go-mysql-server against the real migrations
state: Cancelled
parent: 1
labels: [spike, benchmark]
created: 2026-08-18T14:11:21Z
updated: 2026-08-18T14:20:21Z
---

## Description

Establish what an existing engine already gives us, as the bar a from-scratch build must clear.

Stand up Dolt's go-mysql-server (Apache-2.0), point it at the real 605 migrations, and record:
- cold start time to accepting connections
- how many of the 605 migrations apply cleanly, and the first failure
- whether Prisma 5.4.2 connects (handshake, mysql_native_password, prepared statements)
- whether a Prisma client round-trip works against a few real models
- behaviour on the known-hard features: generated columns, window functions, WITH RECURSIVE, ST_Distance_Sphere

Deliverable: pass rate, timings, and a list of gaps. This is the option that could ship in days, so it deserves a fair test.

## Notes

### 2026-08-18T14:20:21Z — Christopher Vachon (user)

Dropped by decision on 2026-08-18: the project is committed to a from-scratch Rust implementation, so benchmarking go-mysql-server as a candidate foundation no longer informs a live decision. Track A (real MySQL baseline) still runs — it defines the performance target regardless of foundation.
