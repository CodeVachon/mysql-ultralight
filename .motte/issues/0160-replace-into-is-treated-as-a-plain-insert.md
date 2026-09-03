---
id: 160
title: REPLACE INTO is treated as a plain INSERT
state: Todo
parent: 9
labels: [executor]
created: 2026-09-03T03:40:02Z
updated: 2026-09-03T03:40:02Z
---

## Description

Found while implementing #0132.

    REPLACE INTO t (id, a) VALUES (1, 'x')   over an existing row 1
    mysql   deletes the old row, inserts the new one, reports 2 affected, LAST_INSERT_ID is the new id
    engine  1062 Duplicate entry

REPLACE is parsed and then run as an INSERT, so any row that collides raises instead of replacing.

A wrong answer rather than a clear error: the statement is legal and the engine refuses it, which is the direction that breaks working SQL. Prisma does not emit REPLACE, but a hand-written migration or seed script may.

Worth measuring together when taken: how many rows REPLACE reports affected (2 for a replacement, 1 for a plain insert), what LAST_INSERT_ID reports after one (#0132 measured: the new id), whether it fires DELETE-side foreign keys, and what it does with an explicit id versus a generated one.
