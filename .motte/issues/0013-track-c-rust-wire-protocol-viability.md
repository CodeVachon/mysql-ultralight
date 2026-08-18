---
id: 13
title: Track C — Rust wire protocol viability
state: Done
parent: 1
labels: [spike, rust]
created: 2026-08-18T14:11:21Z
updated: 2026-08-18T15:09:53Z
---

## Description

Prove the single riskiest assumption of the from-scratch path: that we can satisfy Prisma's client without MySQL's source.

Build the smallest possible Rust server that:
- completes the MySQL 8 handshake with capability negotiation
- authenticates via mysql_native_password
- answers SELECT 1 over both the text and prepared-statement protocols
- is reached successfully by Prisma 5.4.2 and by the mysql CLI

Measure cold start. Stated language preference is Rust; this track is what earns it.

## Notes

### 2026-08-18T14:41:17Z — Christopher Vachon (user)

Proven on 2026-08-18. Prisma 5.4.2 connects, prepares, binds binary params, transacts, and runs prisma db push against the from-scratch Rust server. mysql2 passes independently. Cold start 1.85ms median spawn-to-accepting over 25 runs (p95 2.10ms); 636K release binary. Full findings in docs/spike-wire-protocol.md. Delivered: crates/protocol (codec, capabilities, auth, handshake, columns, values) and crates/server (connection state machine, command dispatch, tracing), 29 tests, clippy and fmt clean.

### 2026-08-18T15:09:53Z — Christopher Vachon (user)

Post-review hardening on 2026-08-18: /code-review at high effort found 11 issues in the Track C output, all fixed with regression tests. Most severe were a client-triggerable integer overflow panic in binary TIME decoding (fatal under panic=abort), silently wrong parameter decoding when a client re-executes without re-sending types, and binary encoding taking its width from the value rather than the declared column type. Common theme: wrong answers rather than errors. Test count 29 to 40. Detail in docs/spike-wire-protocol.md.
