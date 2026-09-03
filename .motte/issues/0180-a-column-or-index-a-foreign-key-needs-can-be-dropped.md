---
id: 180
title: A column or index a foreign key needs can be dropped
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T12:29:35Z
updated: 2026-09-03T19:15:27Z
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

## Notes

### 2026-09-03T19:15:27Z — Christopher Vachon (user)

Landed in 666676a, gate green. The rule has two halves with two codes: 1828 for the referencing column, 1829 for the referenced one (naming the child table). They sit on opposite sides of the operation loop, which is measurable -- 1828 beats 1091 while 1829 loses to it.

Full precedence, every pair measured both ways round: 1054 > 1090 > 1828 > {1060, 1091} > 1072 > 1829 > 1553.

An index may be dropped when another can serve the key, and the substitute need not be unique -- a plain KEY covers a UNIQUE's job -- but it must be a LEADING prefix: KEY (pid,x) backs FK (pid), KEY (x,pid) does not. foreign_key_checks=0 changes none of it, which is worth knowing because it is the first thing people try.

One shape still gets this wrong and is filed as #0208: an FK over the table's own primary key gets a redundant index here, so DROP PRIMARY KEY is accepted where 8.3 is 1553.
