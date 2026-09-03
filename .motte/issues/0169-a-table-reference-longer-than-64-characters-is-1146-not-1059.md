---
id: 169
title: A table reference longer than 64 characters is 1146, not 1059
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T10:30:29Z
updated: 2026-09-03T14:20:31Z
---

## Description

Found while applying code-review findings to #0107.

MySQL applies the 64-character identifier limit to a table name *written in a query*, before it looks the name up. Measured on 8.3:

    SELECT * FROM `<65>`                              1059 Identifier name '...' is too long
    INSERT INTO `<65>` VALUES (1)                     1059
    SELECT * FROM probe.`<65>`                        1059
    SELECT 1 FROM t WHERE i IN (SELECT 1 FROM `<65>`) 1059
    WITH `<65>` AS (SELECT 1 AS x) SELECT * FROM `<65>`  1059

This engine answers **1146 Table '...' doesn't exist** for the first two and *accepts* the rest.

Note what is NOT limited, because a fix must not widen into it -- all measured, all answer on 8.3:

    WITH `<65>` AS (SELECT 1 AS x) SELECT 1     -- defining a CTE name is fine; an
                                                -- unreferenced CTE is not evaluated at all
    SELECT 1 FROM t AS `<65>`                   -- a table alias carries no limit
    SELECT `<65>` FROM t                        -- a *column* reference is 1054, not 1059

So the limit is on the table reference specifically, not on names generally. An earlier draft of #0107's test doc said 'the CTE's own name is limited', which measurement contradicts -- the 1059 people attribute to the CTE definition is really the FROM that references it. That comment has been corrected to point here.

Why it was not fixed inside #0107: there is no single funnel. `no_such_table` has 13-ish call sites and `Engine::qualify` covers only two of them, so this is a pass over the table-resolution paths rather than a line. Doing it inside `no_such_table` is tempting -- a name over 64 characters can never be in the catalog, since DDL now refuses to create one, so every such lookup must fail -- but the name reaching there is sometimes qualified, and splitting it there to find the part would guess at a boundary the caller already knew.
