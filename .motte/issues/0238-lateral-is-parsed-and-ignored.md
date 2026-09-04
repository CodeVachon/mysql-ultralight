---
id: 238
title: LATERAL is parsed and ignored
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-04T08:10:22Z
updated: 2026-09-04T08:10:22Z
---

## Description

Found while implementing #0230.

    SELECT k FROM Cl, LATERAL (SELECT c3.id AS d FROM Cr JOIN Cr c3 ON c3.id = Cl.id) t
    mysql   2 rows
    engine  1054

Only JSON_TABLE is routed through lateral_join; the LATERAL keyword on a derived table is parsed and discarded, so the derived table cannot see its FROM siblings.

#0230 made the distinction matter: a derived table INSIDE a correlated subquery now answers (an outer reference is legal at any depth), while a LATERAL sibling in the same FROM still does not. Both shapes are in the corpus so the pair cannot collapse silently -- this issue is the half that remains.
