---
id: 99
title: The 1054 message names the wrong clause
state: In Progress
parent: 9
labels: [compat]
created: 2026-08-25T14:48:39Z
updated: 2026-09-03T08:26:12Z
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
