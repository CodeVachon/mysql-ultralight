---
id: 65
title: A UNION with a NULL branch loses the column's type
state: Todo
parent: 9
labels: [compat]
created: 2026-08-21T13:08:59Z
updated: 2026-08-21T13:08:59Z
---

## Description

SELECT id FROM N UNION SELECT NULL ORDER BY 1
      type of column 0   mysql MYSQL_TYPE_LONG   engine MYSQL_TYPE_VAR_STRING
      row 1              mysql Int(1)            engine "1"

The set-operation merge in crates/engine/src/exec.rs widens the two branches and falls back to a string where there is no wider number. A NULL branch has no type to widen against, so it takes that fallback — but NULL is not a competing type, it is the absence of one, and MySQL keeps the INT and only relaxes the nullability.

The row difference is the same cause seen from the other side: declared VAR_STRING, the value goes out as text rather than as an integer, so a binary client reads "1" where MySQL sends Int(1).

Three of the 75 baseline lines.
