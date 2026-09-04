---
id: 123
title: UNION does not unify a decimal's scale across branches
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-26T03:49:12Z
updated: 2026-09-03T23:20:40Z
---

## Description

Found while probing GROUP BY over a document during #0071, and nothing to do with JSON.

    SELECT 2.50 UNION ALL SELECT 2.5 UNION ALL SELECT 2
    mysql   2.50 / 2.50 / 2.00
    engine  2    / 2.50 / 2.5

MySQL types the union column by aggregating the branches -- the widest scale wins -- and renders every row at that scale. This engine renders each branch at its own.

Related to #0065, which taught a UNION to keep a column's type when one branch is NULL; this is the same aggregation applied to a decimal's scale rather than to its type. Worth checking precision alongside scale, and whether a mixed exact/approximate union follows the same rule.

## Notes

### 2026-09-03T14:01:18Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. SELECT 2.50 UNION ALL SELECT 2.5 UNION ALL SELECT 2 gives 2.50/2.50/2.00 on 8.3 and 2.50/2.5/2 here.

### 2026-09-03T23:20:39Z — Christopher Vachon (user)

Landed in f7be76b, gate green. UNION types the column by aggregating the branches and WRITES THE VALUES at that scale -- it is not a rendering rule. Proof it is the values: SELECT DISTINCT x FROM (SELECT 1/3 AS x UNION ALL SELECT 0.3333) t is ONE row on 8.3, which only happens if the wide quotient was actually narrowed to 0.3333 rather than merely displayed that way.

Measured: 2.50/2.5/2 -> 2.50, 2.50, 2.00; 1/3 UNION 2.5 UNION 2 -> 0.3333, 2.5000, 2.0000 with the integer branch padded; four branches take the widest wherever it sits. A double or string branch takes the column instead and there is nothing to unify.
