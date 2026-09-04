---
id: 214
title: The parser accepts reserved words as identifiers
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T23:20:40Z
updated: 2026-09-03T23:20:40Z
---

## Description

Found independently by two agents on 2026-09-03 while implementing #0118/#0122/#0123 and #0084/#0121.

    SELECT 1 AS both / AS groups / AS empty
    mysql   ERROR 1064 -- all three are reserved in 8.3
    engine  accepted

So a query using a reserved word as an alias or column name passes here and is a syntax error against a real server -- the same class as #0185 (IF EXISTS accepted where MySQL rejects it).

sqlparser has a reserved-word list; the question is whether MySqlDialect's matches 8.3's. Worth checking the whole list rather than adding these three.
