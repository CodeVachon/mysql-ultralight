---
id: 24
title: "Executor gaps: GROUP BY, UNION, window functions"
state: Done
parent: 6
labels: [executor]
created: 2026-08-18T18:52:38Z
updated: 2026-08-18T19:16:06Z
---

## Description

What the executor still refuses, each with a clear ER_NOT_SUPPORTED_YET rather than a wrong answer.

GROUP BY with aggregates. Aggregates work over a whole result but not per group, so any grouped query errors. The conformance corpus shows 31 GROUP BY occurrences, and the application's report feeds lean on it.

UNION and UNION ALL, which appear throughout the report feed queries.

Window functions. OVER(PARTITION BY ...) appears 8 times across the migrations and application code, all ROW_NUMBER for deduplication.

Also outstanding in the same area: CHECK constraint enforcement, generated columns, foreign key enforcement at write time, and real transactions. BEGIN and COMMIT are currently accepted and nothing rolls back, which is safe only because the engine discards state at process exit.

## Notes

### 2026-08-18T19:16:06Z — Christopher Vachon (user)

Implemented 2026-08-18. GROUP BY over one or more columns with HAVING, aggregates folded inside larger expressions so SUM(a) / COUNT(*) works without special-casing the arithmetic, COUNT(DISTINCT x), and NULLs grouped together rather than treated as never-equal. UNION and UNION ALL, with a column-count check and dedup sharing the collation that WHERE uses. Window functions: ROW_NUMBER, RANK, DENSE_RANK and aggregates over OVER (PARTITION BY ... ORDER BY ...), including the deduplication idiom the migrations use.

ORDER BY over an aggregate needed its own handling: it names a value that only exists once the group is folded, so sort keys are computed alongside the grouped output rather than afterwards against the projected row.

Still refused with a clear error: INTERSECT and EXCEPT, window frames, named windows. Also still outstanding in this area are CHECK enforcement, generated columns, foreign key enforcement at write time, and real transactions.
