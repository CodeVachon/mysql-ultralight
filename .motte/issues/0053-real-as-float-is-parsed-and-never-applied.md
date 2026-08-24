---
id: 53
title: REAL_AS_FLOAT is parsed and never applied
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T19:24:50Z
updated: 2026-08-24T18:02:04Z
---

## Description

SqlMode::real_as_float in crates/engine/src/dialect.rs is set by SqlMode::parse and asserted by a test, and then nothing ever reads it. REAL is unconditionally a DOUBLE, which is MySQL's behaviour with the flag off — so the default is right and only the flag is a lie.

The module doc says these flags "change what a query *means* rather than how it behaves", which is the reason they are collected at all. A field that advertises a behaviour it does not have is worse than an absent one: `SET sql_mode='REAL_AS_FLOAT'` is accepted and silently ignored.

Either apply it in SqlType::from_ast, where the D::Real arm chooses between "float" and "double", or delete the field and stop accepting the flag. Found while fixing #0041.

## Notes

### 2026-08-24T18:02:04Z — Christopher Vachon (user)

Done in 4205f57. Applied rather than deleted: dialect.rs's own APPELLO constant leads with REAL_AS_FLOAT, so deleting the field would have given the one deployment this engine emulates the wrong type for every REAL. It reaches the code through the per-statement Origin, the route PIPES_AS_CONCAT uses, because there is no text to rewrite.

Measured on 8.3: the flag narrows REAL and REAL UNSIGNED to four bytes at DDL time and narrows CAST(x AS REAL) -- which raises 1690 on 1e300, and MySQL says 'as float' in the message even though REAL was written -- and does not reach DOUBLE PRECISION or its unsigned form. REAL(M,D) is unreachable: sqlparser 0.62's DataType::Real carries no precision.

Applying it exposed the compound spelling. SELECT @@sql_mode after SET sql_mode='ANSI' on 8.3 is REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,ONLY_FULL_GROUP_BY,ANSI, and parse set three of the five. The dropped ONLY_FULL_GROUP_BY is live -- exec.rs raises 1055 off it -- so ANSI was accepting a non-grouped column MySQL rejects. The unit test now asserts the whole struct, because the bug was the flags it failed to set.

TRADITIONAL still has no arm, deliberately: strict is dead, and probing showed the engine refuses an out-of-range write under every mode including none, so an arm would set a field nothing reads -- the defect this issue exists to remove.

Corpus 302 -> 312, baseline empty.
