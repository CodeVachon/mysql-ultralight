---
id: 21
title: ALTER TABLE RENAME INDEX, and DROP EVENT
state: Done
parent: 4
labels: [compat, parser]
created: 2026-08-18T15:51:57Z
updated: 2026-08-18T18:52:11Z
---

## Description

Two small parser gaps found by the conformance extraction, grouped because both are cheap.

ALTER TABLE ... RENAME INDEX old TO new, 6 statements. sqlparser-rs parses RENAME only as a table rename and rejects the INDEX form.

DROP EVENT IF EXISTS, 1 statement. There is no CREATE EVENT anywhere in the codebase, so this drops a legacy scheduled event that no longer exists. Supporting DROP EVENT IF EXISTS as a no-op is sufficient; the event scheduler itself is out of scope.

## Notes

### 2026-08-18T18:52:11Z — Christopher Vachon (user)

Implemented 2026-08-18. ALTER TABLE RENAME INDEX is recognised before parsing and renames the catalog index, erroring with 1176 when the key does not exist. DROP EVENT IF EXISTS is accepted as a no-op, which is sufficient: no CREATE EVENT exists anywhere in the target codebase.
