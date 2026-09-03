---
id: 197
title: CTE column metadata names a database and table MySQL leaves empty
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T15:50:35Z
updated: 2026-09-03T15:50:35Z
---

## Description

Found while implementing #0192, and true of any CTE, recursive or not.

MySQL reports an empty Database and Org_table for a CTE's columns, and the anchor's width. The engine reports the current database, the CTE name, and its widened type -- Length 20 for a bigint where 8.3 sends the anchor's narrower width.

Invisible to the differential harness, which does not compare these on a column whose org_table is empty (see crates/differential/src/compare.rs). So it needs a unit test rather than a corpus case, or the harness widened first.

Related to #0188/#0189/#0190, which are the same family of width questions from other directions.
