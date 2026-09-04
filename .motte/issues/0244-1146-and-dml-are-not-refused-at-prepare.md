---
id: 244
title: 1146 and DML are not refused at prepare
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T08:53:05Z
updated: 2026-09-04T08:53:05Z
---

## Description

Found while implementing #0227, which closed the subquery half of prepare-time resolution. Two categories it deliberately did not cover.

A missing table is not refused at prepare at all:
    SELECT * FROM NoSuch     mysql 1146 at prepare   engine prepares with zero columns
Same for a missing table inside a subquery.

And DML is not DESCRIBED at all, so nothing in it is refused at prepare:
    UPDATE Full SET k = 1 WHERE zz = 1     mysql 1054 at prepare   engine prepares
    DELETE ... WHERE zz = 1, UPDATE ... SET k = zz, INSERT ... VALUES (zz)   same
They ARE refused at execute (#0100's work), so this is about the round trip rather than the answer. Engine::describe returns Ok(None) for anything that is not a Statement::Query.

Priority 2: Prisma prepares every statement, and a migration or query naming a missing table gets no error until it runs.
