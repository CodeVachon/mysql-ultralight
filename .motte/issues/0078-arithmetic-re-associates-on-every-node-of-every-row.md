---
id: 78
title: Arithmetic re-associates on every node of every row
state: Todo
parent: 9
labels: [executor]
created: 2026-08-24T17:36:11Z
updated: 2026-08-24T17:36:11Z
---

## Description

Regression introduced by #0052 in 846c925, found by review during #0053.

precedence() returns a non-zero level for + - * / % DIV, so reassociated() no longer short-circuits the way tightness() let it. Every arithmetic node now deep-clones its operands, flattens the chain and rebuilds it by precedence climbing -- per node, per row -- where before the common case returned immediately.

Measured by the reviewer: a 12-term chain over 20k rows went 9.1 ms -> 1294 ms (142x); at 24 terms, 15 ms -> 5913 ms (391x).

The rebuild is correct and has to stay; what is missing is the early exit. A chain whose operators are already in non-increasing precedence order left to right is the tree sqlparser would have built anyway, so it can be detected and returned untouched without allocating. The check is cheap and the common case -- a + b, a * b + c -- hits it.

This engine's whole premise is start-up and per-statement latency, so this is a defect rather than a tuning note.
