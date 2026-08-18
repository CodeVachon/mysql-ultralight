---
id: 28
title: "Remaining SQL surface: INTERSECT, EXCEPT, window frames"
state: Todo
parent: 6
labels: [executor]
created: 2026-08-18T20:17:13Z
updated: 2026-08-18T20:17:13Z
---

## Description

What the executor still refuses, each with a clear ER_NOT_SUPPORTED_YET rather than a wrong answer. None appears in the target corpus, so these are listed for completeness rather than as blockers.

INTERSECT and EXCEPT. UNION and UNION ALL are supported; the other set operators are rejected before either branch runs.

Window frames (ROWS/RANGE BETWEEN) and named windows. ROW_NUMBER, RANK, DENSE_RANK and frameless aggregates over OVER (PARTITION BY ... ORDER BY ...) all work; a frame clause is refused.

Also unimplemented and worth naming: LAG, LEAD and the other positional window functions, and multi-table UPDATE and DELETE, which the migrations use 45 times and which currently error.
