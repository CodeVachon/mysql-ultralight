---
id: 24
title: "Executor gaps: GROUP BY, UNION, window functions"
state: Todo
parent: 6
labels: [executor]
created: 2026-08-18T18:52:38Z
updated: 2026-08-18T18:52:38Z
---

## Description

What the executor still refuses, each with a clear ER_NOT_SUPPORTED_YET rather than a wrong answer.

GROUP BY with aggregates. Aggregates work over a whole result but not per group, so any grouped query errors. The conformance corpus shows 31 GROUP BY occurrences, and the application's report feeds lean on it.

UNION and UNION ALL, which appear throughout the report feed queries.

Window functions. OVER(PARTITION BY ...) appears 8 times across the migrations and application code, all ROW_NUMBER for deduplication.

Also outstanding in the same area: CHECK constraint enforcement, generated columns, foreign key enforcement at write time, and real transactions. BEGIN and COMMIT are currently accepted and nothing rolls back, which is safe only because the engine discards state at process exit.
