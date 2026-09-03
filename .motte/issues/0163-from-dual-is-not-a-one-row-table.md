---
id: 163
title: FROM DUAL is not a one-row table
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T04:07:42Z
updated: 2026-09-03T14:20:31Z
---

## Description

Found while implementing #0158.

    SELECT 1 FROM DUAL          mysql 1 row   engine 1146 Table 'DUAL' doesn't exist
    SELECT 1 FROM DUAL WHERE 0  mysql 0 rows  engine 1146

MySQL treats DUAL as a one-row table with no columns, so every FROM DUAL statement answers exactly as the FROM-less form does. This engine looks it up in the catalog and refuses.

A clear refusal rather than a wrong answer, which is why #0158 left it -- but SELECT 1 FROM DUAL is a commoner probe shape than SELECT 1 WHERE 0, and it is what several connection libraries emit to check a connection is alive.

The fix should route it to the same path #0158 just made correct, rather than creating a table: DUAL is not case-sensitive, cannot be aliased in MySQL (SELECT 1 FROM DUAL d is a syntax error -- verify), and cannot be joined.

## Notes

### 2026-09-03T14:01:19Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed exactly. SELECT 1 FROM DUAL is one row on 8.3 and 1146 here; FROM DUAL WHERE 0 is zero rows there and 1146 here.
