---
id: 233
title: An unnamed foreign key's auto index is named after the constraint
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T04:38:06Z
updated: 2026-09-04T04:38:06Z
---

## Description

Found while implementing #0205/#0206, measured on 8.3.

    CREATE TABLE midt (pid INT, FOREIGN KEY (pid) REFERENCES P(id))   -- constraint unnamed
    information_schema.statistics.INDEX_NAME
    mysql   'pid'          -- named after the first column
    engine  'midt_ibfk_1'  -- named after the invented constraint

A NAMED constraint agrees on both.

Distinct from #0183 (an unnamed FK's invented CONSTRAINT name can collide) -- this is the INDEX name it creates, not the constraint name. Both come from the same place in apply_constraint.
