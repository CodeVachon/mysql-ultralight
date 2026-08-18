---
id: 28
title: "Remaining SQL surface: INTERSECT, EXCEPT, window frames"
state: Done
parent: 6
labels: [executor]
created: 2026-08-18T20:17:13Z
updated: 2026-08-18T21:18:33Z
---

## Description

What the executor still refuses, each with a clear ER_NOT_SUPPORTED_YET rather than a wrong answer. None appears in the target corpus, so these are listed for completeness rather than as blockers.

INTERSECT and EXCEPT. UNION and UNION ALL are supported; the other set operators are rejected before either branch runs.

Window frames (ROWS/RANGE BETWEEN) and named windows. ROW_NUMBER, RANK, DENSE_RANK and frameless aggregates over OVER (PARTITION BY ... ORDER BY ...) all work; a frame clause is refused.

Also unimplemented and worth naming: LAG, LEAD and the other positional window functions, and multi-table UPDATE and DELETE, which the migrations use 45 times and which currently error.

## Notes

### 2026-08-18T20:28:04Z — Christopher Vachon (user)

Implemented 2026-08-18. Multi-table UPDATE and DELETE now write back through a join: the relation carries the provenance of each row, so a materialised join can be traced to the base rows it came from. That was the gap that mattered — the migrations use multi-table UPDATE 57 times and DELETE with JOIN 3 times, and parsing them was never the same thing as running them. Each base row is updated at most once however many join rows it appears in, as MySQL does.

INTERSECT and EXCEPT join UNION, all three with ALL. LAG, LEAD, FIRST_VALUE and LAST_VALUE join the window functions, with LAST_VALUE following MySQL's default frame, which ends at the current row rather than the end of the partition.

Still refused with a clear error: explicit window frames, named WINDOW clauses and NTH_VALUE. A named WINDOW clause was being parsed and silently ignored, which answered a different query than the one asked; it is now rejected.

### 2026-08-18T21:18:33Z — Christopher Vachon (user)

Reviewed twice. The second review of the fixes found ten more findings, all fixed with regression tests; docs/prototype.md records them. The worst repeated the first pass's mistake one level down: two aliases of one table are two join sources over one row vector, so a per-source delete applied the second set's pre-deletion indices to an already-shortened table and destroyed rows the statement never selected.

Two wrong answers this work made reachable rather than introduced are also fixed: an out-of-scope column qualifier fell back to an unqualified lookup instead of reporting 1054, which with joined DML drives writes against the wrong rows; and a windowed aggregate with ORDER BY folded the whole partition instead of running to the end of the current row's peer group, so SUM(v) OVER (ORDER BY id) returned the grand total on every row.

Refused rather than implemented, each with a clear error: ORDER BY, LIMIT and USING on UPDATE and DELETE, tracked as #0030.
