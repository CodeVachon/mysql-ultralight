---
id: 205
title: A renamed table does not carry its foreign keys with it
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T18:51:28Z
updated: 2026-09-04T04:48:45Z
---

## Description

Found while implementing #0179/#0180/#0184, which fixed the COLUMN half of this and left the TABLE half.

    ALTER TABLE p RENAME TO p2
    children keep REFERENCED_TABLE_NAME = 'p'   where 8.3 says 'p2'

So the constraint points at a table that no longer exists and is unenforceable. Measured on 8.3.

The column half IS fixed: RENAME COLUMN now travels to the foreign keys that point at it, in both directions. This is the same propagation for the table name.

Priority 2 because the target application's migrations run RENAME TO three times and declare 824 foreign keys, so a rename over a parent is plausible.

Related and measured at the same time: REFERENCED_TABLE_NAME is stored as written, so REFERENCES differential.QualP(id) reports 'differential.QualP' here and 'QualP' on 8.3 -- same in key_column_usage and referential_constraints.

## Notes

### 2026-09-04T04:48:45Z — Christopher Vachon (user)

Landed in afbfcbd, gate green, differential 4852/0. The rename half needed the parent stored as TWO fields rather than one dotted string -- that join was lossy (a table genuinely called `a.b` is indistinguishable from a.b once joined), so ForeignKey gained a schema field and FORMAT_VERSION went 11 -> 12, appended at the end of the record.

Storing it apart also fixed something inert: a qualified key had been resolving its parent inside the CHILD's own database, so a cross-database key was never enforced and a same-database qualified one bound by luck.

A rename rewrites the auto-generated <table>_ibfk_N names, and the prefix match is BYTE-EXACT rather than case-insensitive -- T_IBFK_1 on a table renamed T -> T2 keeps its name on 8.3, and so does tc_ibfk_5 on Tc. The backing index is never renamed. Measured both ways round.

Not covered, filed as #0232: RENAME TABLE (the standalone statement) and ALTER TABLE t RENAME TO otherdb.t2, which silently creates a table literally named 'otherdb.t2'.
