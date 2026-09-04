---
id: 245
title: An unreferenced CTE and a comma-joined LATERAL are refused where MySQL answers
state: Done
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T08:53:05Z
updated: 2026-09-04T22:08:26Z
---

## Description

Two shapes found while implementing #0227/#0228, both of them THE ENGINE REFUSING LEGAL SQL -- the worse direction.

    WITH c AS (SELECT zz.q FROM Cr) SELECT 1
    mysql   prepares and answers -- an unreferenced CTE is never resolved
    engine  1054
    (same for WITH c AS (SELECT SIGN() FROM Cr) SELECT 1 -> 1582)

    SELECT * FROM Cl, LATERAL (SELECT Cl.id AS x FROM Cr) d
    mysql   answers three columns
    engine  1054 'Cl.id' in 'field list'

Both pre-existing. #0192 established that 8.3 does not run OR resolve an unreferenced recursive CTE, transitively -- this is the same rule for a non-recursive one, which the engine resolves eagerly.

The LATERAL case is distinct from #0238 (LATERAL parsed and ignored) in that the failure is at resolution rather than evaluation, and it was re-confirmed still diverging after #0230 landed: that fix is in the ON path, this one is lateral_join's derived subquery.

## Notes

### 2026-09-04T21:13:46Z — Christopher Vachon (user)

Implementation COMPLETE and preserved on branch worktree-agent-a047704b24d72035d (ac5e241, rebased onto c8d250e), but NOT merged: it overflows the stack at 20 levels of scalar-subquery nesting and aborts the test suite. Blocker filed as #0250 with the diagnosis needed. Returned to Todo rather than left In Progress, since no agent holds it.

### 2026-09-04T22:08:26Z — Christopher Vachon (user)

Landed in c5b89e2. Both halves were the engine REFUSING SQL MySQL accepts -- an unreferenced CTE (which 8.3 does not resolve at all, matching #0192's finding for the recursive case) and a comma-joined LATERAL. That is the direction that costs a test which would have passed in production.
