---
id: 159
title: A non-numeric string compared to a number answers wrongly
state: Todo
parent: 9
labels: [executor]
created: 2026-09-03T03:22:29Z
updated: 2026-09-03T14:01:19Z
---

## Description

Found while implementing #0139, text-side and pre-existing -- binary now shares the behaviour rather than differing from it.

    SELECT 'abc' = 0     mysql 1      engine 0
    SELECT 'nan' = 0     mysql 1      engine NULL

MySQL converts a string to a number for the comparison, and a string with no leading number converts to 0, so 'abc' = 0 is true. This engine answers 0 for that and, worse, NULL for 'nan' -- a three-valued-logic violation, since neither operand is NULL and the comparison is decidable.

The NULL is the part that matters: a caller cannot distinguish 'unknown' from 'false', and NULL propagates through AND/OR differently from 0, so the wrongness spreads.

Related and distinct: #0141 (a hex or bit literal is not read as a number, so X'0F' = 15 is 0 here and 1 on 8.3) is the same comparison from the binary side. #0139's comment names both so the numeric rule is not read more broadly than it holds.

## Notes

### 2026-09-03T14:01:19Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. 'abc'=0 mysql 1 engine 0; 'nan'=0 mysql 1 engine NULL.
