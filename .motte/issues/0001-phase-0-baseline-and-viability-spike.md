---
id: 1
title: Phase 0 — Baseline and viability spike
state: Done
labels: [epic, phase-0]
created: 2026-08-18T14:10:54Z
updated: 2026-08-18T18:52:38Z
---

## Description

The foundation question is settled: a from-scratch Rust implementation. Rust 1.97.1 (aarch64-apple-darwin) is installed and validated. What remains is to establish the performance target and de-risk the riskiest technical assumption before committing to an architecture.

Track A — Real MySQL baseline. Measure the status quo so every later claim of 'faster' has a number behind it: docker compose up of mysql:8.3, prisma migrate deploy over 605 migrations, and per-test-file cost in the existing jest suite.

Track C — Rust wire protocol viability. Prove that a hand-written Rust MySQL server can satisfy Prisma 5.4.2's client: handshake, capability negotiation, mysql_native_password, and SELECT 1 over both the text and prepared-statement protocols.

Plus extraction of the machine-readable conformance surface from the target codebase, which scopes every later phase.

Track B (go-mysql-server evaluation) was cancelled on 2026-08-18 — see #0012.

Exit criteria: the architecture decision record at #0015.

## Notes

### 2026-08-18T18:52:38Z — Christopher Vachon (user)

Phase 0 complete 2026-08-18. Track A baselined real MySQL at 4.5-8.9s container start plus 26.1s migrate deploy; Track B was cancelled once Rust was committed to; Track C proved the wire protocol at 1.85ms cold start; the conformance surface was extracted and the ADR written.
