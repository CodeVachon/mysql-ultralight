---
id: 237
title: Functional dependency on a primary key is not recognised
state: Todo
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T08:10:22Z
updated: 2026-09-04T08:10:22Z
---

## Description

Found while implementing #0229, where it confounded several probes until every selected column was grouped.

    SELECT k AS b, COUNT(*) FROM Cl GROUP BY id ORDER BY b     -- id is the PRIMARY KEY
    mysql   3 rows -- k is functionally dependent on the PK, so ONLY_FULL_GROUP_BY allows it
    engine  1055

MySQL's ONLY_FULL_GROUP_BY permits an ungrouped column when the GROUP BY determines it -- via a primary key or a unique NOT NULL key. The engine requires every selected column to be grouped.

Priority 2: this is ordinary Prisma-shaped SQL (group by the id, select the row's other columns), and the engine refuses a statement a real server answers -- the failure direction this project exists to prevent.

Related: the 1055 message text also differs from MySQL's for every 1055 -- 8.3 names the expression's POSITION, the qualified column, and says 'which is not functionally dependent on columns in GROUP BY clause'. Invisible to the differential, which compares code and SQLSTATE for failures.
