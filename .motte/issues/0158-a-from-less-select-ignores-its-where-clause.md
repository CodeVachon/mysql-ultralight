---
id: 158
title: A FROM-less SELECT ignores its WHERE clause
state: In Progress
parent: 9
labels: [executor]
created: 2026-09-03T02:43:34Z
updated: 2026-09-03T02:58:24Z
---

## Description

Found by review during #0116, pre-existing and unrelated to that issue.

    SELECT 1 WHERE 0        mysql returns no rows   engine returns one row
    SELECT 1 WHERE 1 = 0    same

The FROM-less SELECT path evaluates its projection and never consults the WHERE clause at all.

A plain wrong answer in one of the simplest statements SQL has, and one a client can easily generate -- SELECT <expr> WHERE <predicate> with no table is a normal shape for a probe or a guard query.

It also masks other refusals: SELECT 1 WHERE 1 IN (SELECT SIGN(1,2)) answers a row rather than raising, and the same statement with a malformed LIKE ... ESCAPE misbehaves identically, because neither the predicate nor anything inside it is ever reached.

Related in shape to #0009's zero-rows note but the opposite direction: there a check never ran because no row was evaluated; here a clause is never evaluated at all, so a row is returned that should not be.

Check GROUP BY, HAVING and ORDER BY on the same path in one pass -- #0103 fixed the FROM-less HAVING, so the projection path clearly reaches some clauses and not others, and it is worth establishing which.
