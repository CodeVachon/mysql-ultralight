---
id: 83
title: A concatenation of two literals is typed nullable
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-08-24T18:02:26Z
updated: 2026-09-03T14:20:26Z
---

## Description

Found while building the ANSI differential group for #0053.

    SELECT 'a' || 'b'   under PIPES_AS_CONCAT   mysql NOT NULL   engine nullable

Pre-existing and never exercised, because no differential group ran under PIPES_AS_CONCAT before #0053 added one. Neither operand can be NULL, so neither can the result, and never_null already reasons about exactly this for other operators -- see #0063, which taught the describe path that a computed column that cannot be NULL is reported NOT NULL.

Kept out of the corpus for the same reason as the information_schema divergence found beside it: it would have cost the new group its empty baseline over a gap that is not that group's subject.

## Notes

### 2026-09-03T14:00:44Z — Christopher Vachon (user)

VALIDATED 2026-09-03 with --column-type-info against mysql:8.3, connection charset pinned to utf8mb4 on both sides. The premise HOLDS and understates the gap.

    SELECT 'a' || 'b'  under PIPES_AS_CONCAT
    Flags     mysql NOT_NULL   engine (none)      <- the documented claim
    Length    mysql 8          engine 4096        <- not in the description
    Decimals  mysql 31         engine 0           <- not in the description

Type (VAR_STRING) and Collation agree once the client charset is pinned. Worth pinning it: the mysql client in the container defaults to latin1, which reports latin1_swedish_ci for every string column and reads as a divergence that is really the client -- the same trap that mis-measured #0074.
