---
id: 53
title: REAL_AS_FLOAT is parsed and never applied
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T19:24:50Z
updated: 2026-08-20T19:24:50Z
---

## Description

SqlMode::real_as_float in crates/engine/src/dialect.rs is set by SqlMode::parse and asserted by a test, and then nothing ever reads it. REAL is unconditionally a DOUBLE, which is MySQL's behaviour with the flag off — so the default is right and only the flag is a lie.

The module doc says these flags "change what a query *means* rather than how it behaves", which is the reason they are collected at all. A field that advertises a behaviour it does not have is worse than an absent one: `SET sql_mode='REAL_AS_FLOAT'` is accepted and silently ignored.

Either apply it in SqlType::from_ast, where the D::Real arm chooses between "float" and "double", or delete the field and stop accepting the flag. Found while fixing #0041.
