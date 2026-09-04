---
id: 240
title: An alias inside a function call is resolvable but not substitutable
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-04T08:10:40Z
updated: 2026-09-04T08:10:40Z
---

## Description

Found while implementing #0229 and deliberately left.

    SELECT 1 AS a, COUNT(*) FROM Cl GROUP BY ABS(a)
    mysql   1 row
    engine  1054

substitute_names reaches an identifier through a binary op, a unary op, a nested expression, IS NULL and BETWEEN -- but not through Function, Case or InList.

Where the query also has an ungrouped column the 1055 hides it (so SELECT k AS a ... GROUP BY ABS(a) now agrees after #0229); where it does not, the row loop raises 1054.

Left alone on purpose: the walker is SHARED with substitute_aliases, and rewriting an aggregate's argument into a hidden alias column would break the fold. So widening it needs the two callers separated first.

Also measured in the same family: GROUP BY (SELECT a) is 2 rows on 8.3 and 1055 here.
