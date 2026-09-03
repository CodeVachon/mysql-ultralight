---
id: 171
title: A duplicate CHECK or FOREIGN KEY constraint name is accepted
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T10:19:37Z
updated: 2026-09-03T14:20:32Z
---

## Description

Found while implementing #0150, and deliberately left out of it: #0150 is the index name space, and these two are the schema-wide one beside it.

Measured on mysql:8.3, all four accepted here:

- `CONSTRAINT ck CHECK (a > 0)` twice in one table, or once in each of two tables in the same schema: 3822 (HY000) `Duplicate check constraint name 'ck'.` — note the trailing full stop, which MySQL prints and most of its messages do not.
- `CONSTRAINT fk1 FOREIGN KEY ...` reused in a *second* table: 1826 (HY000) `Duplicate foreign key constraint name 'fk1'`.

Reused inside one table, a foreign key name is already 1061 after #0150, because the index MySQL creates for the key collides first. The 1826 path is the one that is left: it needs a lookup across every table in the database, which nothing in `apply_constraint` has today — it is handed one table.

A check constraint name is schema-scoped like a foreign key's, and an index name is not.
