---
id: 239
title: A WHERE inside a derived table cannot see the enclosing row
state: Done
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T08:10:22Z
updated: 2026-09-04T22:08:26Z
---

## Description

Found while implementing #0230, which fixed the ON but not the WHERE.

    SELECT k FROM Cl WHERE EXISTS (SELECT 1 FROM (SELECT id AS z FROM Cr WHERE Cr.id = Cl.id) d)
    mysql   answers
    engine  1054 in 'where clause'

#0230 reaches the ON through session.lateral, but a derived table's WHERE resolves from the 'outer' parameter, which relation_of's Derived arm does not pass down. Fixing it means threading session.lateral into the inner query as its outer row.

Priority 2: 29 subqueries in FROM and 72 EXISTS subqueries in the target application, and a correlated WHERE inside a derived table is a common shape.

## Notes

### 2026-09-04T21:13:46Z — Christopher Vachon (user)

Implementation COMPLETE and preserved on branch worktree-agent-a047704b24d72035d (ac5e241, rebased onto c8d250e), but NOT merged: it overflows the stack at 20 levels of scalar-subquery nesting and aborts the test suite. Blocker filed as #0250 with the diagnosis needed. Returned to Todo rather than left In Progress, since no agent holds it.

### 2026-09-04T22:08:26Z — Christopher Vachon (user)

Landed in c5b89e2 with #0243 and #0245 -- one mechanism, three symptoms. #0230 carried the enclosing row's VALUES into evaluation, #0227 carried its NAMES into resolution, and neither carried its TYPES; a derived table's own WHERE got none of them. All three now reach the same depth.
