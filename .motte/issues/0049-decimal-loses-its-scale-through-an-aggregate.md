---
id: 49
title: DECIMAL loses its scale through an aggregate
state: Todo
parent: 9
labels: [executor, compat]
created: 2026-08-20T17:57:30Z
updated: 2026-08-20T17:57:30Z
---

## Description

Split out of #0040, which is about the type a column is *described* with. This is about the value.

MySQL keeps a DECIMAL's scale through an aggregate: SUM over a DECIMAL(10,2) of 1.50 and 2.50 is 4.00, and AVG of the same is 2.000000 - AVG adds four digits of scale to the argument's, which is its documented rule. This engine answers 4 and 2, because a DECIMAL is held as text and folded through f64, so the scale is whatever the arithmetic left behind.

Visible in the harness as row divergences on SELECT k, COUNT(*), SUM(v), MIN(s), MAX(v), AVG(v) FROM G GROUP BY k, in both protocols.

Separate from #0040 because describing the column as NEWDECIMAL - which it now is - does not make the value right, and fixing the value does not need the type work. It probably needs decimal arithmetic that tracks scale rather than going through a float, which is its own piece of work.
