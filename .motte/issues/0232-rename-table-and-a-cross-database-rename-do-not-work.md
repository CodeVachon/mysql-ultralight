---
id: 232
title: RENAME TABLE and a cross-database rename do not work
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T04:38:06Z
updated: 2026-09-04T04:38:06Z
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
