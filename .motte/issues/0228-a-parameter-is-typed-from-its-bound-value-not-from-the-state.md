---
id: 228
title: A parameter is typed from its bound value, not from the statement
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T04:21:03Z
updated: 2026-09-04T09:14:41Z
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

## Notes

### 2026-09-04T09:14:41Z — Christopher Vachon (user)

Landed in 9e43c6d. The rule is one sentence: a parameter has no type of its own; it takes the one its position gives it. With nothing to take it is VAR_STRING at 65532 bytes (Item_param's 65535 as 16383 utf8mb4 characters). In arithmetic it takes the other operand's kind. In a conditional it takes the branch that HAS a type, at the widest of that kind -- COALESCE(?, i) over an INT is LONGLONG, not LONG.

TWO CLIENT TRAPS had to be worked around to measure it at all, both worth knowing for any future prepare-time work: the mysql crate's statement cache must be disabled or a repeated statement returns metadata refreshed from the EXECUTE response; and one statement the client cannot read (SELECT i FROM T ORDER BY ?) DESYNCHRONISES THE SOCKET and silently corrupts every answer after it, so each measurement needs a fresh connection.

A regression this change introduced was caught by review: typing a parameter as a string made COALESCE/IFNULL/IF/CASE/GREATEST VAR_STRING where they had been LONGLONG.

Remainders filed as #0243 (correlated reference typing) and #0246 (the parameter definition block).
