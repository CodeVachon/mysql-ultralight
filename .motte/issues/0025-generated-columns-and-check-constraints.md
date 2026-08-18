---
id: 25
title: Generated columns and CHECK constraints
state: Done
parent: 6
labels: [executor]
created: 2026-08-18T19:51:39Z
updated: 2026-08-18T20:01:39Z
---

## Description

Both are recorded in the catalog today but not evaluated or enforced.

Generated columns: 3 in the migrations, each of the same shape, used to build a unique key that only applies to live rows. For example TimeSheetRecord.activeUserDateKey is CASE WHEN isDeleted = false THEN CONCAT(userId, '-', workYear, '-', workMonth, '-', workDay) END, which makes the uniqueness conditional. Without evaluation the column is always NULL, and a NULL never conflicts, so the constraint the schema is expressing does not exist.

CHECK constraints: 3 in the migrations. Currently CREATE TABLE with one is refused outright.

## Notes

### 2026-08-18T20:01:39Z — Christopher Vachon (user)

Implemented 2026-08-18. Generated columns are evaluated on INSERT and UPDATE from the row's other values, writing to one is rejected with 3105, and the conditional-unique-key idiom the migrations use is covered by a test: a key that is NULL for deleted rows, so uniqueness applies only to live ones. CHECK constraints are enforced on both write paths and pass when the expression is true or unknown, only an outright false violating, as in MySQL. Both go through one finalise_row step shared by INSERT and UPDATE, because a constraint enforced on one path and not the other is worse than one enforced on neither.
