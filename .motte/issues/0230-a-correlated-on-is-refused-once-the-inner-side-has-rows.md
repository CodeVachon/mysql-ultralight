---
id: 230
title: A correlated ON is refused once the inner side has rows
state: Todo
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T04:21:03Z
updated: 2026-09-04T04:21:03Z
---

## Description

Found while implementing #0100, pre-existing.

    SELECT k FROM Cl WHERE EXISTS (SELECT 1 FROM Cr JOIN Cr c3 ON c3.id = Cl.id)
    with Cr non-empty:  mysql answers,  engine 1054 'on clause'

The evaluator builds the ON's row context from the join's columns alone, so a reference to the enclosing query's row is unresolvable at evaluation time. #0100 fixed the RESOLVE-time half of this (its new ON check carries session.lateral's schema, after a review caught it refusing the empty case), but the evaluator still cannot see the enclosing row.

Fixing it means carrying the enclosing row into build_relation.

Priority 2: 72 EXISTS subqueries and 307 LEFT JOINs in the target application; a correlated join inside an EXISTS is a common shape.
