---
id: 117
title: Unary plus is not peeled from a column's name
state: Done
parent: 9
labels: [compat]
created: 2026-08-25T23:13:29Z
updated: 2026-09-02T19:07:21Z
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

## Notes

### 2026-09-02T19:07:21Z — Christopher Vachon (user)

Done in 4d8789f, in an isolated worktree.

#0059 already peeled Nested and unary plus for a literal, which is why SELECT +1 was correct; this extends the peel to a column reference. Minus is never peeled and is unchanged -- SELECT -1 is named -1, SELECT - 1 keeps its space.

Two real behaviours came with it, both found in review, because the plus was being read as a sign rather than as nothing. Inside a wrapper it is indistinguishable from its operand while a minus computes: NULLIF(+f,1) over a FLOAT is FLOAT where minus gives DOUBLE, NULLIF(+tn,1) over a TINYINT is TINY where minus gives LONGLONG, and CASE WHEN 1 THEN +u END over an INT UNSIGNED keeps the flag where minus drops it.

A position is read through the plus too: ORDER BY +1, ORDER BY + +1, ORDER BY +(1) and GROUP BY +1 all name the first output column on 8.3, while ORDER BY -1 is a constant that sorts nothing, and an out-of-range ORDER BY +2 reports the digits rather than +2.

That last is not cosmetic, and the first test for it did not bite -- substitute_names reached the name anyway. The case that settles it is an alias shadowing a real column: over Sh(id, b), 8.3 answers ORDER BY +b by the ALIAS and ORDER BY b + 0 by the COLUMN, opposite row orders, and without the peel the engine gave the column's order for both. A silent wrong ordering the obvious test could not see. SELECT DISTINCT id AS b ... ORDER BY +b answers on 8.3 while ORDER BY -b is 3065, which is the biting case for the check_distinct_order site.

The agent also caught a hazard of its own: a corpus case using ORDER BY -1 returned rows in undefined order, which the README forbids. Rewritten as ORDER BY -1, id -- still discriminating, since as a position it would sort differently.

15 mutations, all bite. One find string covers two mechanisms and attribution stays clean because a position is a literal and never reaches column_parts.

Merged on top of #0140; the branch's statement count was relative to its own base and the merged corpus is 2514, verified by running the harness rather than trusting either arithmetic. 0 divergences, baseline empty.
