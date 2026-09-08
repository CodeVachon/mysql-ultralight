---
id: 258
title: A derived table's column list is checked after its subquery has run
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-08T15:40:17Z
updated: 2026-09-08T15:40:17Z
---

## Description

Found by review of #0256 and then measured, which changed the answer.

`aliased_column_names` is called after `query_outer` has already run the derived table, so an error raised while producing its rows preempts the 1353 the list would have raised.

    SELECT * FROM (SELECT 9223372036854775807 + 1 AS q) AS d(x, y)
    mysql   1353  the list is the wrong length
    engine  1690  BIGINT value is out of range

MySQL resolves the query expression -- and so the shape the list is checked against -- before any row is produced.

THE CTE SPELLING IS THE OPPOSITE, AND THAT IS MEASURED, NOT ASSUMED:

    WITH w(x, y) AS (SELECT 9223372036854775807 + 1, 2) SELECT 1 FROM w
    mysql   1690
    engine  1690

So 8.3 does NOT apply one order to both. The derived table checks its list first; the CTE runs first. The engine currently matches on the CTE and diverges on the derived table, and "fixing" both together would break the half that is already right. Anyone working this should re-measure both before changing either.

What it needs: the derived path has to know how many columns the subquery returns without running it. The shaping path (`SHAPING`, used by `describe`) already types a query without producing rows, so the machinery exists; wiring it in for this one check is the work.

Priority 3: both statements are errors either way, so this decides which code a client sees for a statement that was already going to fail. Nothing in the target application reaches it -- its 604 migrations apply, and the corpus is clean at 5598 statements because the cases in it do not also error at runtime.
