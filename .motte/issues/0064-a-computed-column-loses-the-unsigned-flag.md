---
id: 64
title: A computed column loses the UNSIGNED flag
state: Todo
parent: 9
labels: [compat]
created: 2026-08-21T13:08:38Z
updated: 2026-08-21T13:08:38Z
---

## Description

Two statements in the differential baseline, four lines, both MySQL UNSIGNED against this engine's signed:

    SELECT id, ROW_NUMBER() OVER (ORDER BY id) FROM G    column 1
    SELECT CAST(n AS CHAR), CAST(t AS UNSIGNED) FROM S   column 1

ROW_NUMBER counts from one and MySQL says so. A CAST names the signedness in the statement itself, which is the easier of the two — crates/engine/src/exec.rs already reads `SIGNED` and `UNSIGNED` out of a cast for the *type* and drops the sign.

Separate from the nullability of the same columns, which has its own issue, because the flag and the mechanism are different: OutColumn carries `declared` for the UNSIGNED flag and `nullable` for the other.

Note that the UNSIGNED lines under `types and description` are not this — those belong to #0050, where information_schema describes its own columns.
