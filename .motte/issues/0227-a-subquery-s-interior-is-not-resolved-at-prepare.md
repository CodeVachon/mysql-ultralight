---
id: 227
title: A subquery's interior is not resolved at prepare
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T04:21:03Z
updated: 2026-09-04T04:21:03Z
---

## Description

The one clause of #0100 that its implementation deliberately did not close. Measured on 8.3 with the mysql crate's conn.prep, cache disabled.

    SELECT (SELECT zz.q FROM Cr) FROM Cl        -- Cl empty
    SELECT id FROM Cl WHERE id IN (SELECT zz.q FROM Cr)
    mysql   1054 'zz.q' in 'field list', AT PREPARE
    engine  describes a column, and only errors once a row reaches it

Same for a bad ESCAPE inside a subquery.

Why it was left: subquery_column swallows the inner probe with .ok() and is a TYPING walker with no way to report an error. Closing it means threading a Result through several walkers, which is a change of shape rather than an added check -- and #0100 was already large.

Priority 2: the target application uses 72 EXISTS subqueries and 29 subqueries in FROM, so a name error inside one is a statement that passes here and fails against a real server.
