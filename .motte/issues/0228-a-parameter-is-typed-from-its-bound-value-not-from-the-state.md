---
id: 228
title: A parameter is typed from its bound value, not from the statement
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T04:21:03Z
updated: 2026-09-04T04:21:03Z
---

## Description

Found while implementing #0109, and the agent judged it larger than #0109's naming half. Measured on 8.3 with the mysql crate's conn.prep, statement cache disabled (the crate refreshes cached columns from the EXECUTE response, so a repeated statement hands back stale prepare metadata -- a trap worth knowing).

8.3 types a parameter from the STATEMENT; this engine types it from the value bound, and NULL when describing:

    SELECT ?            mysql VAR_STRING len 65532 dec 31 cs 45 nullable
                        engine MYSQL_TYPE_NULL len 0 dec 0 cs 63
    SELECT ?+1          mysql LONGLONG len 22 dec 0
                        engine DOUBLE len 23 dec 31
    SELECT CONCAT(?,?)  mysql MEDIUM_BLOB len 524256
                        engine VAR_STRING len 0

This is real parameter type inference, not a naming rule. Prisma prepares every statement, so the describe response is what it reads to decide how to decode.

Not reachable by the differential harness: Answer::from_binary binds nothing, so a parameterised case fails client-side before any metadata is compared. It needs engine tests or a purpose-built probe, which is how #0109 was measured.
