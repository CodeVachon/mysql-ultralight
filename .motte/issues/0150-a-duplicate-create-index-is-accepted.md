---
id: 150
title: A duplicate CREATE INDEX is accepted
state: In Progress
parent: 9
labels: [compat]
created: 2026-09-01T06:09:09Z
updated: 2026-09-03T09:52:21Z
---

## Description

Found while implementing #0137/#0138, unrelated to them.

Creating an index whose name already exists on the table is accepted here; MySQL refuses it with 1061 Duplicate key name.

The same shape as the duplicate-column check #0125 had to hoist out of the CTAS path: a name the engine will answer only one of, accepted twice. Worth checking ALTER TABLE ADD INDEX and ADD CONSTRAINT in the same pass, and whether a duplicate index name across two different tables is legal (it is -- an index name is scoped to its table, unlike a constraint name).
