---
id: 67
title: A function's scale argument is read as part of its type
state: Todo
parent: 9
labels: [compat]
created: 2026-08-21T13:08:59Z
updated: 2026-08-21T13:08:59Z
---

## Description

SELECT ROUND(NULL, 1)   mysql MYSQL_TYPE_DOUBLE   engine MYSQL_TYPE_LONGLONG
    SELECT MOD(NULL, 2)     mysql MYSQL_TYPE_DOUBLE   engine MYSQL_TYPE_LONGLONG

Two different causes with one symptom. `ROUND` is typed by `combined` over *all* its arguments in crates/engine/src/exec.rs, so the `1` that says how many places to keep is read as if it were a value being rounded — with a NULL first argument the scale is the only type left and it decides. Only the first argument should count.

`MOD` is in the fixed-name table as a LONGLONG, which is right for two integers and wrong for a NULL: MySQL types anything with no numeric kind of its own as a DOUBLE, which is what `numeric_answer` beside it already does for ABS and ROUND.

Four of the 75 baseline lines. Found while auditing the baseline against the issue list rather than by the harness reporting anything new.
