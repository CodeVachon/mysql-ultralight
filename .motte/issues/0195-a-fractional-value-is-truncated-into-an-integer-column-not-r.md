---
id: 195
title: A fractional value is truncated into an integer column, not rounded
state: In Progress
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T15:50:35Z
updated: 2026-09-03T16:32:40Z
---

## Description

Found while implementing #0192 and confirmed with no CTE involved.

    CREATE TABLE Ints (i INT);
    INSERT INTO Ints VALUES (1.5),(2.5),(-1.5);
    SELECT i FROM Ints ORDER BY i
    mysql   -2, 2, 3      (rounds half away from zero)
    engine  -1, 1, 2      (truncates towards zero)

Wrong stored data, silently, on an ordinary INSERT. Note 8.3 rounds half AWAY from zero here (2.5 -> 3, -1.5 -> -2), which is not the half-to-even rule #0124 records for shortest-form doubles -- the two are different mechanisms and should not be conflated.

Through a recursive CTE the same gap stops progress entirely: a TINYINT anchor at 100 with a +0.5 step is 100,101,102,103 on 8.3 and 3636 here, because the value never advances.
