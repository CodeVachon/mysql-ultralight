---
id: 117
title: Unary plus is not peeled from a column's name
state: In Progress
parent: 9
labels: [compat]
created: 2026-08-25T23:13:29Z
updated: 2026-09-02T14:25:12Z
---

## Description

MySQL names `SELECT +a` after the operand — `a` — where this engine uses the written text, `+a`. Measured on 8.3 over a DECIMAL(10,2) column:

    SELECT +a       mysql `a`        engine `+a`
    SELECT +(a)     mysql `a`        engine `+(a)`
    SELECT + a      mysql `a`        engine `+ a`
    SELECT +1       mysql `1`        engine `1`
    SELECT +TRUE    mysql `+TRUE`    engine `+TRUE`

The literal cases already agree, from #0059. What does not is a *column reference* under a unary plus: #0059's recorded rule is that parentheses and unary plus are peeled and then the item carries a parse-time name only if what remains is a literal, otherwise the whole unpeeled item is named by its source text — and the first three rows above say the peeling applies to a column reference too.

`crates/differential/src/cases.rs` aliases `+d AS plus_d` in the temporal group for exactly this, with a comment calling it a naming gap; the new `decimal arithmetic` group leaves `+a` out for the same reason.

Found while measuring for #0070.
