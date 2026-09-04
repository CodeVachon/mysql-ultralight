---
id: 243
title: A correlated column reference is typed VAR_STRING
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T08:53:05Z
updated: 2026-09-04T08:53:05Z
---

## Description

Found while implementing #0227/#0228, measured with the mysql crate's conn.prep.

    SELECT (SELECT Cl.id FROM Cr) FROM Cl
    mysql   LONG len 11
    engine  VAR_STRING len 4096

Cause: direct_column maps a slot into 'input', which is this query's relation only, so an OUTER column's slot falls past it and the type is lost. Fixing it means threading the outer relation's TYPES down, not just its ColumnRefs -- #0230 carries the outer row's VALUES for evaluation and #0227 carries its NAMES for resolution, but neither carries its types.

It forced a corpus case to be reshaped during #0227: SELECT (SELECT Sq.id FROM SqR) FROM Sq diverged on TYPE rather than on the refusal it was written to assert, so it became a WHERE ... IS NULL form with the reason in a comment.

Priority 2: a correlated scalar subquery is ordinary SQL and Prisma reads the described type to decode.
