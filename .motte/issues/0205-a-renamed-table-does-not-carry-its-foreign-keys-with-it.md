---
id: 205
title: A renamed table does not carry its foreign keys with it
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T18:51:28Z
updated: 2026-09-03T18:51:28Z
---

## Description

Found while implementing #0179/#0180/#0184, which fixed the COLUMN half of this and left the TABLE half.

    ALTER TABLE p RENAME TO p2
    children keep REFERENCED_TABLE_NAME = 'p'   where 8.3 says 'p2'

So the constraint points at a table that no longer exists and is unenforceable. Measured on 8.3.

The column half IS fixed: RENAME COLUMN now travels to the foreign keys that point at it, in both directions. This is the same propagation for the table name.

Priority 2 because the target application's migrations run RENAME TO three times and declare 824 foreign keys, so a rename over a parent is plausible.

Related and measured at the same time: REFERENCED_TABLE_NAME is stored as written, so REFERENCES differential.QualP(id) reports 'differential.QualP' here and 'QualP' on 8.3 -- same in key_column_usage and referential_constraints.
