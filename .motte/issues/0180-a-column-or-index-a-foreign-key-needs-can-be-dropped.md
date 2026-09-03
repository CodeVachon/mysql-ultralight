---
id: 180
title: A column or index a foreign key needs can be dropped
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T12:29:35Z
updated: 2026-09-03T14:20:33Z
---

## Description

Found while implementing #0133, pre-existing. Two halves of one rule, both measured on 8.3:

    ALTER TABLE t DROP COLUMN pid      -- pid backs fk1
    mysql  1828 HY000 Cannot drop column 'pid': needed in a foreign key constraint 'fk1'
    engine accepted

    ALTER TABLE t DROP INDEX uq        -- the FK relies on that index
    mysql  1553 HY000 Cannot drop index 'uq': needed in a foreign key constraint
    engine accepted

Both leave the catalog holding a foreign key whose backing is gone, which MySQL will not let a schema reach.
