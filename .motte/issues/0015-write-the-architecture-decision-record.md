---
id: 15
title: Write the architecture decision record
state: Todo
parent: 1
labels: [spike, decision]
blockedBy: [11, 13, 14]
created: 2026-08-18T14:11:21Z
updated: 2026-08-18T14:20:21Z
---

## Description

Synthesize Track A (real MySQL baseline), Track C (Rust wire protocol viability) and the conformance surface extraction into the founding architecture document for the Rust implementation.

Contents: measured performance target derived from the real MySQL baseline, proven-or-disproven wire protocol approach, crate layout for the workspace, the storage/executor design implied by the state-image boot requirement, and the ordered feature roadmap derived from the conformance checklist.

Blocked until Tracks A and C and the surface extraction report. Closing this unblocks Phase 1 onward.
