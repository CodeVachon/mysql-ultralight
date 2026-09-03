---
id: 99
title: The 1054 message names the wrong clause
state: Done
parent: 9
labels: [compat]
created: 2026-08-25T14:48:39Z
updated: 2026-09-03T11:38:49Z
---

## Description

Found while implementing #0058, which made the field list and WHERE cases match 8.3 exactly.

Every other clause still says 'field list':

    GROUP BY a.b   mysql 'group statement'        engine 'field list'
    HAVING a.b     mysql 'having clause'          engine 'field list'
    ORDER BY a.b   mysql 'order clause'           engine 'field list'
    JOIN ... ON    mysql 'on clause'              engine 'field list'
    OVER (PARTITION BY a.b)  mysql 'window partition by'
    OVER (ORDER BY a.b)      mysql 'window order by'

crates/engine/src/exec.rs's Context::lookup hardcodes the clause because it does not know which one it is resolving in. Fixing it means threading the clause through the resolver.

Cosmetic in isolation, but the message is the whole product of an error path, and a client that matches on it -- or a person reading it -- is told to look in the wrong place.

## Notes

### 2026-09-03T11:38:49Z — Christopher Vachon (user)

Landed in ef49012. Threading the clause through the resolver exposed a second bug that was invisible while every clause claimed to be 'field list': 8.3 resolves the select list before the rest of the body, and this engine resolved it late. Only visible with a fault in two clauses at once. resolve_field_list moved ahead of the WHERE. A join's ON is still out of place and belongs to #0100. Also collapsed ~22 hand-built Context literals into Context::for_row(..).with_executor(..), which is most of the 215 lines removed and means a mutation neuters every call site rather than one of twenty.
