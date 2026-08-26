---
id: 123
title: UNION does not unify a decimal's scale across branches
state: Todo
parent: 9
labels: [compat]
created: 2026-08-26T03:49:12Z
updated: 2026-08-26T03:49:12Z
---

## Description

Found while probing GROUP BY over a document during #0071, and nothing to do with JSON.

    SELECT 2.50 UNION ALL SELECT 2.5 UNION ALL SELECT 2
    mysql   2.50 / 2.50 / 2.00
    engine  2    / 2.50 / 2.5

MySQL types the union column by aggregating the branches -- the widest scale wins -- and renders every row at that scale. This engine renders each branch at its own.

Related to #0065, which taught a UNION to keep a column's type when one branch is NULL; this is the same aggregation applied to a decimal's scale rather than to its type. Worth checking precision alongside scale, and whether a mixed exact/approximate union follows the same rule.
