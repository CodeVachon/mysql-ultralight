---
id: 184
title: RENAME TO mid-statement skips the operations after it
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T12:30:07Z
updated: 2026-09-03T19:15:27Z
---

## Description

Found while implementing #0133, pre-existing and documented in the code but not fixed there.

The ALTER TABLE RENAME TO arm writes to the catalog and returns, so anything written after it in the same statement is skipped in silence:

    ALTER TABLE t RENAME TO t2, DROP INDEX nosuch
    mysql  1091, and the table is still called t
    engine renames it and never reads the second operation

Written the other way round the engine now agrees, because #0133 made the drop refuse. This is the one place the all-operations-checked-before-any-applied rule is bypassed -- the rename needs hoisting out of the loop so the statement is settled before any of it is applied.

## Notes

### 2026-09-03T19:15:27Z — Christopher Vachon (user)

Landed in 666676a, gate green. RENAME TO is now recorded during the loop and applied after the statement is settled, so it no longer skips whatever follows it, and the all-operations-checked-before-any-applied rule holds everywhere. It also refuses 1050 onto an existing table with both tables left intact, and a column rename now travels to the foreign keys that point at it.

The TABLE half of that propagation is not done -- a renamed parent leaves children pointing at the old name -- and is filed as #0205.
