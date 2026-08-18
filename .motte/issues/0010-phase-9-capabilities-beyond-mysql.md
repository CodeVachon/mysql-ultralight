---
id: 10
title: Phase 9 — Capabilities beyond MySQL
state: Todo
labels: [epic, phase-9]
blockedBy: [6]
created: 2026-08-18T14:10:54Z
updated: 2026-08-18T14:11:33Z
---

## Description

Things a purpose-built test engine can do that real MySQL cannot, requested for v1.

Deterministic time: freeze or advance NOW() and CURRENT_TIMESTAMP per connection so time-dependent assertions are stable without app-layer mocking.

Deterministic IDs: control auto-increment sequences.

Query introspection: expose the SQL a test actually executed, with plans, so tests can assert on query behavior and catch N+1s.

Compatibility flags: honour --sql-require-primary-key to match the production MySQL configuration, so violations surface in tests rather than production.
