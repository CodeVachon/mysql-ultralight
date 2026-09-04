---
id: 215
title: CAST ignores a declared string or binary width
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T01:36:11Z
updated: 2026-09-04T01:36:11Z
---

## Description

Found while implementing #0201, measured on 8.3. One gap with two spellings.

    HEX(CAST('a' AS BINARY(3)))      mysql 610000 (padded)   engine 61
    HEX(CAST('abcd' AS BINARY(3)))   mysql 616263 (truncated) engine 61626364
    CAST('abcd' AS CHAR(3))          mysql 'abc'             engine 'abcd'

Bare CAST('abc' AS BINARY) agrees, so it is specifically the declared width that functions::cast ignores.

#0201 fixed CAST for DECIMAL, where the task asked for it, and deliberately did NOT half-fix this one: CAST truncates where a column write refuses (1406), so the two paths have different semantics and this needs its own measurement rather than reusing the write path's rule.

Priority 2 because the target application calls CAST 27 times.
