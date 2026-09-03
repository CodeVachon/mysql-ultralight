---
id: 196
title: A computed column carries no declared type, so CAST loses its scale
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T15:50:35Z
updated: 2026-09-03T15:50:35Z
---

## Description

Found while implementing #0192.

    WITH RECURSIVE t AS (SELECT CAST(1 AS DECIMAL(10,2)) AS n
                         UNION ALL SELECT n+1 FROM t WHERE n<3)
    SELECT n FROM t
    mysql   1.00, 2.00, 3.00
    engine  1.00, 2, 3

The same CTE over a real DECIMAL(10,2) *column* agrees, which is what identifies the cause: a computed column carries no declared SqlType, so the CAST's scale is not available to the rows that follow.

Deliberately not in the differential corpus, since baseline.txt must stay empty.
