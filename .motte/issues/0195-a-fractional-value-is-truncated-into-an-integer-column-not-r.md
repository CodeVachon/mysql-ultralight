---
id: 195
title: A fractional value is truncated into an integer column, not rounded
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T15:50:35Z
updated: 2026-09-03T18:11:19Z
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

## Notes

### 2026-09-03T18:11:19Z — Christopher Vachon (user)

Landed in 780f620, gate green. Rounding turned out to be THREE rules, which only measuring finds: a DECIMAL rounds half away from zero (1.5,2.5,3.5 -> 2,3,4), a DOUBLE rounds half to EVEN (1.5e0,2.5e0,3.5e0 -> 2,2,4), and a string is read as an exact decimal ('2.5' -> 3). Range is checked AFTER rounding, so 127.6 into a TINYINT is 1264 rather than 128. An UNSIGNED reads the sign before rounding for decimals only.

sql_mode decides error vs clamp, and TRADITIONAL had to be added to the mode parser as part of this -- strict was inert before, so nothing had noticed that MySQL's strictest mode parsed to the loosest setting here.
