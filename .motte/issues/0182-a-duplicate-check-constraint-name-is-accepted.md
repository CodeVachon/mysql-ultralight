---
id: 182
title: A duplicate CHECK constraint name is accepted
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T12:30:06Z
updated: 2026-09-03T12:30:06Z
---

## Description

Found while implementing #0133. The same shape as #0150, one constraint kind further along.

    CREATE TABLE t (a INT, b INT, CONSTRAINT ck1 CHECK (a > 0));
    ALTER TABLE t ADD CONSTRAINT ck1 CHECK (b > 0)
    mysql  3822 HY000 Duplicate check constraint name 'ck1'.
    engine accepted

Worse than a wrong error: with #0133 landed, one DROP CONSTRAINT ck1 then removes both of them, so a name the engine let you create twice cannot be dropped once.

Note a check constraint name is schema-scoped in MySQL, not table-scoped like an index name -- #0150 measured 3822 for a reuse in another table too.
