---
id: 144
title: ORDER BY resolves a shadowed column before a select-list alias
state: Todo
parent: 9
labels: [executor]
created: 2026-09-01T02:16:08Z
updated: 2026-09-01T02:16:08Z
---

## Description

Found by new corpus cases while implementing #0103, and reproduces with no HAVING anywhere.

    SELECT id AS b FROM Hv ORDER BY b
    mysql   sorts by the alias   1,2,3,4
    engine  sorts by the column b   4,1,2,3

crates/engine/src/exec.rs's sort_rows runs against the relation before the projection exists, so a name that is both a select-list alias and a real column of the table resolves to the column. MySQL resolves an ORDER BY name against the select list first and only then the FROM clause.

A wrong answer rather than a refusal, and a quiet one -- every row is present and plausibly ordered.

Related to #0090, which is the positional form of the same clause resolving against the wrong thing (ORDER BY 1 sorts by the input column rather than the projection). Whoever takes one should take the other: both are sort_rows running too early to see what the query actually projects.

The corpus case that found this was reshaped so the baseline could stay empty, with the reason written above it.
