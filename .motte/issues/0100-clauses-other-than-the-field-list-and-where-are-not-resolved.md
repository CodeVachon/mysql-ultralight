---
id: 100
title: Clauses other than the field list and WHERE are not resolved at prepare
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-25T14:48:39Z
updated: 2026-09-04T04:21:19Z
---

## Description

The remainder of #0058, deliberately left rather than rushed.

#0058 resolves a qualifier that is not in scope in the field list, in a qualified wildcard and in WHERE. Still unchecked at prepare, and still answering over an empty relation where MySQL refuses: GROUP BY, HAVING, ORDER BY, a join ON condition, the interior of an OVER spec, and the interior of any subquery.

Left because refusing something legal is the worse failure and each of these has a trap. ORDER BY and HAVING admit output aliases, which are not columns of any relation in scope. The OVER spec needs its own clause name in the message (see the clause-name issue). A subquery's interior can correlate to the outer query, which is exactly the shape #0058's resolver is careful never to walk into.

The mechanism is already there -- unresolved_column and refused_at_prepare in crates/engine/src/exec.rs -- so this is about extending it clause by clause, each with the alias and correlation rules measured first.

## Notes

### 2026-09-03T04:51:36Z — Christopher Vachon (user)

#0102 (in progress) found DML's ORDER BY is still resolved per matching row and left it here deliberately, because this issue owns ORDER BY and fixing it there would close this one silently.

Measured on 8.3 while doing that work:

    UPDATE Bare SET k=1 WHERE id=9 ORDER BY zz.q LIMIT 1   mysql 1054 'order clause'   engine 0 rows
    with a matching row present                             mysql 1054 'order clause'   engine 1054 'field list'
    UPDATE LE SET s='x' ORDER BY s LIKE 'a' ESCAPE '!!'     mysql 1210                  engine answers

So dml_row_order needs both a name check and an ESCAPE guard, and the clause name in the message is 'order clause' rather than the 'field list' it currently reports.

The full DML error ordering #0102 measured, which this fits into: 1146 missing table, then 1066/1109 for a repeated or unknown multi-DELETE target, then the WHERE left to right with names and ESCAPE interleaved positionally, then SET targets, then SET values, then ORDER BY last.

### 2026-09-03T14:05:52Z — Christopher Vachon (user)

NOT re-measured in the 2026-09-03 validation pass. The claims involve resolution at PREPARE time, which the text-protocol probe used for that pass cannot reach -- it needs a client that prepares a statement without executing it. Same for #0109 (a bound parameter's column name) and #0131 (a binary-protocol f32 rendering). Those three need the differential harness's binary path or a mysql2 client rather than the mysql CLI.

### 2026-09-04T04:21:19Z — Christopher Vachon (user)

ADVANCED, not closed, by b996f04 (gate green, differential 4802/0).

Measured on 8.3 with the mysql crate's conn.prep: 8.3 resolves EVERY clause at prepare -- GROUP BY as 'group statement', HAVING, ORDER BY, a join's ON, a window's 'window partition by'/'window order by', and the same for UPDATE/DELETE/INSERT. A bad ESCAPE is 1210 and wrong arity 1582 in each. The clause names at prepare are identical to the ones #0099 measured at execute.

Now closed: GROUP BY, join ON, window PARTITION BY/ORDER BY, DML ORDER BY. Already right beforehand: field list, WHERE, HAVING, SELECT ORDER BY, derived tables.

STILL OPEN and split out as #0227: a subquery's interior. subquery_column swallows the inner probe with .ok() and is a typing walker with nowhere to report an error, so it needs a Result threaded through several walkers.

Also learned and worth keeping: 8.3 resolves POSITIONALLY within a clause, not names-first. An earlier comment in this codebase asserted names-first without measuring; measurement says ON nope = 1 AND s LIKE 'a' ESCAPE '!!' is 1054 and the reverse order is 1210.
