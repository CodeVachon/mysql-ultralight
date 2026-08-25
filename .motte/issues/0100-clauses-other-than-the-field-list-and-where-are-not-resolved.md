---
id: 100
title: Clauses other than the field list and WHERE are not resolved at prepare
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T14:48:39Z
updated: 2026-08-25T14:48:39Z
---

## Description

The remainder of #0058, deliberately left rather than rushed.

#0058 resolves a qualifier that is not in scope in the field list, in a qualified wildcard and in WHERE. Still unchecked at prepare, and still answering over an empty relation where MySQL refuses: GROUP BY, HAVING, ORDER BY, a join ON condition, the interior of an OVER spec, and the interior of any subquery.

Left because refusing something legal is the worse failure and each of these has a trap. ORDER BY and HAVING admit output aliases, which are not columns of any relation in scope. The OVER spec needs its own clause name in the message (see the clause-name issue). A subquery's interior can correlate to the outer query, which is exactly the shape #0058's resolver is careful never to walk into.

The mechanism is already there -- unresolved_column and refused_at_prepare in crates/engine/src/exec.rs -- so this is about extending it clause by clause, each with the alias and correlation rules measured first.
