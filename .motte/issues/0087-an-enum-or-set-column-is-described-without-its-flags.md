---
id: 87
title: An ENUM or SET column is described without its flags
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T03:02:00Z
updated: 2026-08-25T03:02:00Z
---

## Description

Found by measurement during #0068.

MySQL reports an ENUM column as MYSQL_TYPE_STRING with ENUM_FLAG | NUM_FLAG set and length 1; a SET column as MYSQL_TYPE_STRING with SET_FLAG | NUM_FLAG. This engine gets the type code right, sets no flags at all, and reports length 4096.

Not caught by the differential harness, and that is the point: crates/differential compares only the NOT_NULL and UNSIGNED flags and does not compare column length, so nothing in the corpus can guard a fix. Doing this properly means widening what the harness compares first -- which is worth doing on its own, since the same blind spot hides every other flag -- and then fixing the engine with a case that bites.

Order matters here: widen the harness first and the fix is provable; fix the engine first and the corpus stays green either way, which is how #0081 happened.
