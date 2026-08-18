---
id: 21
title: ALTER TABLE RENAME INDEX, and DROP EVENT
state: Todo
parent: 4
labels: [compat, parser]
created: 2026-08-18T15:51:57Z
updated: 2026-08-18T15:51:57Z
---

## Description

Two small parser gaps found by the conformance extraction, grouped because both are cheap.

ALTER TABLE ... RENAME INDEX old TO new, 6 statements. sqlparser-rs parses RENAME only as a table rename and rejects the INDEX form.

DROP EVENT IF EXISTS, 1 statement. There is no CREATE EVENT anywhere in the codebase, so this drops a legacy scheduled event that no longer exists. Supporting DROP EVENT IF EXISTS as a no-op is sufficient; the event scheduler itself is out of scope.
