---
id: 67
title: A function's scale argument is read as part of its type
state: Done
parent: 9
labels: [compat]
created: 2026-08-21T13:08:59Z
updated: 2026-08-21T17:26:57Z
---

## Description

SELECT ROUND(NULL, 1)   mysql MYSQL_TYPE_DOUBLE   engine MYSQL_TYPE_LONGLONG
    SELECT MOD(NULL, 2)     mysql MYSQL_TYPE_DOUBLE   engine MYSQL_TYPE_LONGLONG

Two different causes with one symptom. `ROUND` is typed by `combined` over *all* its arguments in crates/engine/src/exec.rs, so the `1` that says how many places to keep is read as if it were a value being rounded — with a NULL first argument the scale is the only type left and it decides. Only the first argument should count.

`MOD` is in the fixed-name table as a LONGLONG, which is right for two integers and wrong for a NULL: MySQL types anything with no numeric kind of its own as a DOUBLE, which is what `numeric_answer` beside it already does for ABS and ROUND.

Four of the 75 baseline lines. Found while auditing the baseline against the issue list rather than by the harness reporting anything new.

## Notes

### 2026-08-21T17:15:44Z — Christopher Vachon (user)

Both fixed; baseline 11 to 7. ROUND and TRUNCATE type by their first argument alone — MySQL ignores the scale argument entirely, which ROUND(1, NULL) being a LONGLONG shows from the other side. MOD routes through arithmetic_type, the same function % uses, and the two now agree on every operand kind. TRUNCATE turned out to be untyped altogether and was answering VAR_STRING.

### 2026-08-21T17:26:57Z — Christopher Vachon (user)

Review found the type had been made right without the value: MOD was typed like % while still reading both sides as integers, so MOD(1.500,2) answered 1 under a column described NEWDECIMAL. It now goes through the operator's own evaluator, so the two cannot drift. TRUNCATE was typed here while the evaluator had never heard of it — implemented, including a negative number of places. Both remaining differences from MySQL are #0070, and they now affect MOD and % equally.
