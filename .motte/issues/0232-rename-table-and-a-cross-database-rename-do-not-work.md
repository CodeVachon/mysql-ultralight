---
id: 232
title: RENAME TABLE and a cross-database rename do not work
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T04:38:06Z
updated: 2026-09-04T21:31:56Z
---

## Description

Found while implementing #0205, which fixed the ALTER TABLE ... RENAME TO form. Two spellings it does not reach:

    RENAME TABLE a TO b
    mysql   renames and propagates to foreign keys
    engine  1235 does not yet support statement 'RENAME TABLE P2'

    ALTER TABLE t RENAME TO otherdb.t2
    mysql   MOVES the table to otherdb, ending with other/p3
    engine  creates a table literally NAMED 'other.p3' in the current database

The second is the worse of the two: it silently produces a table whose name contains a dot, which nothing can address correctly afterwards.

#0205 made the parent's schema and name separate fields (FORMAT_VERSION 12) precisely so a qualified name is never re-joined and re-split, so the machinery to do this correctly now exists.

## Notes

### 2026-09-04T21:31:56Z — Christopher Vachon (user)

Landed in 4a2357b, gate green, differential 5366/0.

RENAME TABLE is now a statement of its own. Three rules were measured and TWO CONTRADICTED my first implementation:
  - pairs apply IN ORDER, so 'A TO Tmp, Tmp TO A3' is legal while 'A TO X, B TO X' is not. Validating against the catalog plus exceptions gets the second and fails the first; only simulating the sequence gets both.
  - the TARGET is checked before the SOURCE, visible only when both are wrong: RENAME TABLE gone TO Have is 1050, not 1146.
  - a rename onto the SAME name is 1050 here, where ALTER TABLE ... RENAME TO treats it as a no-op. Both spellings are now in the corpus so neither drifts into the other.

1146 names the table qualified with its database, which a SELECT's 1146 does not.

The cross-database move is refused rather than half-implemented and is filed as #0251. Before this, the ALTER form built a table literally named 'other.Alt' in the current database -- the rows existed under a name nothing could address.

The corpus caught a case this issue is not about: a rename differing only in case is 1146 on the reference server (lower_case_table_names=0) and found here. Removed rather than blessed, with the reason recorded in place -- that is #0151.
