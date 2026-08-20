---
id: 42
title: DIV and the arithmetic operators MySQL accepts
state: Done
parent: 9
labels: [executor]
created: 2026-08-20T14:21:40Z
updated: 2026-08-20T19:42:30Z
---

## Description

Found by the differential harness. SELECT ROUND(n / 3, 2), ABS(n), n DIV 2, n % 3 FROM S is answered by MySQL and refused here with 1235.

The refusal is at least clear rather than wrong, which is the convention working. DIV is the integer-division operator; it is worth checking which of ROUND, ABS and % are involved before assuming it is only DIV.

## Notes

### 2026-08-20T19:42:29Z — Christopher Vachon (user)

Only DIV was involved; ROUND, ABS and % already worked. DIV needed its parse tree re-associated, because sqlparser's dialect hook reads the right operand with parse_expr and swallows the rest of the expression. Also fixed: arithmetic on a non-numeric string answered NULL where MySQL reads a leading number, and ABS/ROUND over an integer were narrowed by #0040. Split out #0051 (MOD infix), #0052 (bitwise), #0054 (temporal arithmetic), #0055 (float rendering).
