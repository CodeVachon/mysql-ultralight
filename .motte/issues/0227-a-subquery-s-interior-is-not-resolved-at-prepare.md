---
id: 227
title: A subquery's interior is not resolved at prepare
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T04:21:03Z
updated: 2026-09-04T09:14:41Z
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

## Notes

### 2026-09-04T09:14:40Z — Christopher Vachon (user)

Landed in 9e43c6d, gate green, differential 5086/0. WIDER than filed: 8.3 refuses at prepare in a scalar subquery, IN, EXISTS, = ANY, > ALL, and in a subquery written into the outer GROUP BY / ORDER BY / HAVING / ON / a function argument / a CASE arm. The clause named in the 1054 is the INNER one, not the outer clause the subquery sits in.

Two shapes would have made the fix REFUSE LEGAL SQL and had to be measured rather than reasoned about: EXISTS throws its select list away before expanding *, so EXISTS (SELECT nope.* FROM Cr) is legal where the same qualifier in a scalar subquery is 1051; and a correlated reference must still resolve, so SELECT (SELECT Cl.id FROM Cr) FROM Cl is legal and ... FROM Full is 1054.

Two review findings were the same bug in join and lateral_join: probing a subquery in a correlated ON against the join's own columns refused what 8.3 answers.

Rebased onto #0229/#0230 by intent -- the ON scope is now a SINGLE mechanism serving both, proved by a mutation that breaks both changes' coverage at once.
