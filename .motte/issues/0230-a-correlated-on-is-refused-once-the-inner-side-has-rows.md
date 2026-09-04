---
id: 230
title: A correlated ON is refused once the inner side has rows
state: Done
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T04:21:03Z
updated: 2026-09-04T08:14:42Z
---

## Description

Found while implementing #0100, pre-existing.

    SELECT k FROM Cl WHERE EXISTS (SELECT 1 FROM Cr JOIN Cr c3 ON c3.id = Cl.id)
    with Cr non-empty:  mysql answers,  engine 1054 'on clause'

The evaluator builds the ON's row context from the join's columns alone, so a reference to the enclosing query's row is unresolvable at evaluation time. #0100 fixed the RESOLVE-time half of this (its new ON check carries session.lateral's schema, after a review caught it refusing the empty case), but the evaluator still cannot see the enclosing row.

Fixing it means carrying the enclosing row into build_relation.

Priority 2: 72 EXISTS subqueries and 307 LEFT JOINs in the target application; a correlated join inside an EXISTS is a common shape.

## Notes

### 2026-09-04T08:14:42Z — Christopher Vachon (user)

Landed in db8294a. The enclosing row is carried into evaluation via a Cow -- borrowed when there is no correlation, so an ordinary join pays nothing per row pair. A CTE inside a correlated subquery gets the outer row too.

One distinction is now pinned in the corpus so it cannot collapse silently: a derived table in the SAME FROM is 1054 without LATERAL, while a derived table INSIDE the subquery answers, because an outer reference is legal at any depth and LATERAL governs only FROM siblings. A review finding claimed the second must also be 1054; measurement refuted it.

Confirmed for #0218's benefit: this fix adds NO recursion level. What changes is that correlated queries which previously aborted at 1054 now run to completion, so a deeply nested correlated EXISTS reaches its natural depth rather than bailing at the first ON.

The WHERE half is not fixed and is filed as #0239: a derived table's WHERE resolves from the 'outer' parameter, which relation_of's Derived arm does not pass down.
