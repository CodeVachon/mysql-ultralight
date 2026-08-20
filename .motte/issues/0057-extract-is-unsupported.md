---
id: 57
title: EXTRACT is unsupported
state: Todo
parent: 9
labels: [executor]
created: 2026-08-20T20:16:48Z
updated: 2026-08-20T20:16:48Z
---

## Description

SELECT EXTRACT(YEAR FROM NOW()) fails with 1235 "does not yet support expression `EXTRACT(YEAR FROM NOW())`". MySQL answers 2026.

sqlparser gives EXTRACT its own Expr::Extract variant rather than a function call, which is why it never reached the evaluator. #0040 already types the variant as LONGLONG — correct, and unreachable until this lands. The unit list is the work: MySQL takes YEAR, QUARTER, MONTH, WEEK, DAY, HOUR, MINUTE, SECOND, MICROSECOND and the compound forms like YEAR_MONTH and DAY_SECOND, most of which crates/engine/src/temporal.rs can already answer.

Found while measuring column names for #0043. A clear error today, not a wrong answer.
