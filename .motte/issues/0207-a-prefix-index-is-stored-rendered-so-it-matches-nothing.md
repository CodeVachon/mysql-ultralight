---
id: 207
title: A prefix index is stored rendered, so it matches nothing
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T18:51:31Z
updated: 2026-09-03T18:51:31Z
---

## Description

Found while implementing #0179/#0180/#0184, pre-existing.

    CREATE INDEX ixp ON t (b(4))
    mysql   COLUMN_NAME 'b', SUB_PART 4
    engine  COLUMN_NAME 'b(4)', SUB_PART NULL

The prefix length is rendered into the column name rather than stored beside it, so the index names a column that does not exist and matches nothing. The 1072 check added for #0179 explicitly steps around this rather than reporting it, because reporting it would refuse a statement 8.3 accepts.

Same shape for a functional index over an expression.
