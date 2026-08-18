---
id: 15
title: Write the architecture decision record
state: Done
parent: 1
labels: [spike, decision]
blockedBy: [11, 13, 14]
created: 2026-08-18T14:11:21Z
updated: 2026-08-18T17:45:38Z
---

## Description

Synthesize Track A (real MySQL baseline), Track C (Rust wire protocol viability) and the conformance surface extraction into the founding architecture document for the Rust implementation.

Contents: measured performance target derived from the real MySQL baseline, proven-or-disproven wire protocol approach, crate layout for the workspace, the storage/executor design implied by the state-image boot requirement, and the ordered feature roadmap derived from the conformance checklist.

Blocked until Tracks A and C and the surface extraction report. Closing this unblocks Phase 1 onward.

## Notes

### 2026-08-18T17:45:38Z — Christopher Vachon (user)

Written 2026-08-18 as docs/adr-001-architecture.md. Rust from scratch on sqlparser-rs, confirmed by the 96.3 percent parse rate. Crate boundaries protocol / server / engine / cli / conformance, with server reaching the engine through the existing Handler trait. Synchronous engine, one handler per connection. sql_mode threaded into the parser from the start. information_schema served as a real queryable schema. Build order recorded in the ADR.
