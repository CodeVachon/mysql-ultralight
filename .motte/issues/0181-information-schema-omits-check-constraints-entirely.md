---
id: 181
title: information_schema omits CHECK constraints entirely
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T12:30:06Z
updated: 2026-09-03T12:30:06Z
---

## Description

Found while implementing #0133, pre-existing.

MySQL lists CHECK rows in information_schema.TABLE_CONSTRAINTS. info_schema.rs:888 lists only unique and primary indexes and foreign keys, so a check constraint the engine is enforcing is invisible to a schema read. information_schema.CHECK_CONSTRAINTS is a stub returning no rows at all (info_schema.rs:58).

Noticed because #0133 had to establish exactly what the DROP CONSTRAINT name space is, and it is precisely what TABLE_CONSTRAINTS lists -- so the engine now enforces a name space it cannot report.
