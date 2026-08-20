---
id: 40
title: Computed expressions are described as strings
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T14:21:40Z
updated: 2026-08-20T14:21:40Z
---

## Description

Found by the differential harness, and the one class of divergence that is a standing decision rather than an oversight.

MySQL types a computed expression: LENGTH(t) is LONGLONG, t LIKE 'H%' is LONGLONG, IFNULL(n,0) is LONGLONG, NULLIF(n,5) is LONG, AVG(v) is NEWDECIMAL, and a JSON constructor is JSON. This engine reports VAR_STRING for all of them, because plan_expr infers nothing about an expression and says so.

Recorded here because the harness will report it on every run until either the inference exists or this is declared permanent. docs/prototype.md already names the limitation for view columns; the same widening is what a client sees for any computed column.

Two consequences worth separating out if this is ever picked up: AVG over a DECIMAL also loses scale in the value, not only the type - MySQL answers 2.000000 where this answers 2 - and SUM over DECIMAL(10,2) answers 4 where MySQL answers 4.00.
