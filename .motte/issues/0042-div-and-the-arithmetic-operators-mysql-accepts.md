---
id: 42
title: DIV and the arithmetic operators MySQL accepts
state: Todo
parent: 9
labels: [executor]
created: 2026-08-20T14:21:40Z
updated: 2026-08-20T14:21:40Z
---

## Description

Found by the differential harness. SELECT ROUND(n / 3, 2), ABS(n), n DIV 2, n % 3 FROM S is answered by MySQL and refused here with 1235.

The refusal is at least clear rather than wrong, which is the convention working. DIV is the integer-division operator; it is worth checking which of ROUND, ABS and % are involved before assuming it is only DIV.
