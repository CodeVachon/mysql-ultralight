---
id: 221
title: SHOW ... WHERE is ignored entirely
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T02:36:38Z
updated: 2026-09-04T02:36:38Z
---

## Description

Found while implementing #0148/#0154. Two faults in one statement.

    SHOW VARIABLES WHERE Variable_name = 'sql_require_primary_key'
    mysql   one row      engine  all 41

    SHOW TABLES WHERE Tables_in_probe = 'APPLE'
    mysql   no rows (that column is case-sensitive)   engine  apple
    ... = 'apple'   one row on both

So the WHERE clause is not applied at all, AND the column it filters on would be case-sensitive if it were -- the same information_schema collation rule #0148 fixed for the catalog views.

Sibling of #0096 (SHOW VARIABLES ignores its LIKE pattern): same statement family, different clause. Worth doing in one pass.
