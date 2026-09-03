---
id: 189
title: MySQL also materialises for DISTINCT, GROUP BY and LIMIT
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T12:52:56Z
updated: 2026-09-03T12:52:56Z
---

## Description

Found while implementing #0110.

#0110 established that MySQL narrows an integer column only where it *materialises*, and that a derived table with a FROM is merged into the reading query and keeps its LONGLONG. But a missing FROM is not the only trigger:

    SELECT * FROM (SELECT ti+1 a FROM Ints GROUP BY 1) z   mysql LONG   engine LONGLONG
    ... LIMIT 1      mysql LONG
    ... DISTINCT     mysql LONG
    ... ORDER BY 1   mysql LONGLONG  (not materialised)
    SELECT 1 MOD 3 UNION SELECT 4    mysql LONGLONG (materialises, never reads a width)

materialised_by_mysql deliberately claims only the FROM-less case. Widening it needs the merge conditions measured properly rather than guessed, which is why #0110 stopped where the measurements stopped rather than risk the empty baseline.
