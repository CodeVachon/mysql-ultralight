---
id: 26
title: Foreign key enforcement
state: Done
parent: 6
labels: [executor]
created: 2026-08-18T19:51:39Z
updated: 2026-08-18T20:01:39Z
---

## Description

735 foreign keys are declared across the migrations and recorded in the catalog for introspection, but nothing enforces them at write time. A test that expects a constraint violation gets a successful write instead, which is the wrong-answer failure this engine exists to avoid.

Needs: reject an INSERT or UPDATE whose referencing columns have no matching parent row, and apply the declared referential action on DELETE and UPDATE of the parent. The corpus uses ON DELETE CASCADE 214 times, ON DELETE SET NULL 318 times and ON UPDATE CASCADE 811 times, so all three paths are exercised.

A NULL in any referencing column exempts the row, as in MySQL.

## Notes

### 2026-08-18T20:01:39Z — Christopher Vachon (user)

Implemented 2026-08-18. A child row must reference an existing parent on INSERT and UPDATE, with a NULL in any referencing column exempting the row as in MySQL. ON DELETE CASCADE, SET NULL and RESTRICT are applied, and cascades chain through a queue rather than one level, so deleting a job removes its timesheets and their records. A constraint whose parent table does not exist is inert rather than failing every write.

Parent keys are snapshotted before the child table is borrowed mutably, so validation is a pure function of the candidate row rather than requiring two simultaneous borrows of the catalog.
