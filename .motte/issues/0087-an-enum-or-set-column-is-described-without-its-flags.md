---
id: 87
title: An ENUM or SET column is described without its flags
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T03:02:00Z
updated: 2026-08-28T17:00:29Z
---

## Description

Found by measurement during #0068.

MySQL reports an ENUM column as MYSQL_TYPE_STRING with ENUM_FLAG | NUM_FLAG set and length 1; a SET column as MYSQL_TYPE_STRING with SET_FLAG | NUM_FLAG. This engine gets the type code right, sets no flags at all, and reports length 4096.

Not caught by the differential harness, and that is the point: crates/differential compares only the NOT_NULL and UNSIGNED flags and does not compare column length, so nothing in the corpus can guard a fix. Doing this properly means widening what the harness compares first -- which is worth doing on its own, since the same blind spot hides every other flag -- and then fixing the engine with a case that bites.

Order matters here: widen the harness first and the fix is provable; fix the engine first and the corpus stays green either way, which is how #0081 happened.

## Notes

### 2026-08-28T17:00:29Z — Christopher Vachon (user)

#0126 (a2e3d47) makes the length half of this provable: the harness now compares column_length for any column MySQL reports with a non-empty org_table, and the engine computes an ENUM's width as the widest member x 4 and a SET's as all members plus commas x 4, both measured on 8.3.

The flag half is one line away. ENUM|SET are named in compare.rs's UNMODELLED set, so those bits are excluded from the flags comparison; removing the two names is what turns this issue's fix into something the corpus can prove. Do that first, measure the divergences it reports, then fix.

Note MySQL sets ENUM_FLAG|NUM_FLAG on an ENUM column and SET_FLAG|NUM_FLAG on a SET column -- but NUM is separately excluded and genuinely incomparable (MySQL sets it on a field a temp table produced, so the same column carries it under GROUP BY and not without), so only the ENUM and SET bits themselves are in scope here.
