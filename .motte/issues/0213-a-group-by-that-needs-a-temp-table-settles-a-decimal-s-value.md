---
id: 213
title: A GROUP BY that needs a temp table settles a decimal's value
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T23:20:40Z
updated: 2026-09-03T23:20:40Z
---

## Description

Found while implementing #0118/#0122/#0123 and deliberately not modelled.

Over DECIMAL(10,2) b with KEY (a), measured on 8.3:

    SELECT JSON_ARRAY(SUM(b/3)) FROM g GROUP BY a      [0.999999999]  -- index serves it, value stays wide
    SELECT JSON_ARRAY(SUM(b/3)) FROM g GROUP BY a+0    [1.000000]     -- temp table, value settled

The engine always keeps it wide, so it matches the indexed case and diverges on the unindexed one.

This is a question about which PLAN MySQL chose, not about what the value is, which is why it was left out of the corpus with the reason written into the group -- the same argument materialised_by_mysql already makes about integer widths (#0189). Modelling it means modelling MySQL's optimiser, which is a much larger claim than this engine makes anywhere else.
