---
id: 70
title: Decimal arithmetic loses the scale
state: In Progress
parent: 9
labels: [executor]
created: 2026-08-21T15:22:20Z
updated: 2026-08-25T21:51:02Z
---

## Description

#0049 gave storage and the aggregates an exact decimal; arithmetic still goes through an f64 and comes back with whatever the float printed. Over a DECIMAL(10,2) a and a DECIMAL(8,3) b, both holding 1.5:

                mysql      engine
    a + b       3.000      3
    a - b       0.000      0
    a * b       2.25000    2.25
    a / b       1.000000   1
    a + 1       2.50       2.5
    a * 2       3.00       3
    a + 0.5     2.00       2
    -a          -1.50      -1.5

MySQL's rules, measured on 8.3: addition and subtraction take the larger of the two scales, multiplication adds them, division adds four to the left one, and a sign keeps it.

Every rule reads off the operands' own scales, and crates/engine/src/decimal.rs already counts a decimal in units of its last place, so the arithmetic can be exact rather than merely rounded at the end. The work is that crates/engine/src/expr.rs::arithmetic converts both sides with as_number and has nowhere to put a scale — it needs a branch for two exact operands, the way the fold in exec.rs now has one.

Not in the differential baseline: the corpus has no decimal arithmetic. Found by measuring while closing #0049.
