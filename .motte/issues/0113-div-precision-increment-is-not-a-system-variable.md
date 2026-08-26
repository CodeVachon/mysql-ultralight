---
id: 113
title: div_precision_increment is not a system variable
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T23:13:03Z
updated: 2026-08-25T23:13:03Z
---

## Description

`SET div_precision_increment = 6` is 1193 `Unknown system variable` here, and `SELECT @@div_precision_increment` is the same. MySQL has it as a session variable with a documented default of 4, and it decides two things this engine hard-codes:

- a quotient's scale, which is the left operand's plus the increment (#0085, fixed with the constant)
- an average's scale, which is the scale it averaged plus the increment (#0049, likewise)

Measured on 8.3: `SET div_precision_increment = 6` makes `1/3` 0.333333 and `1.5/1.5` 1.0000000; at 0 it makes `1/3` 0 and `7/2` 3 — the second is 3 rather than 4 because MySQL computes the quotient to a whole number of nine-digit words and truncates there, and at an increment of 0 there are no words at all. `DIV` and `MOD` are unaffected at any setting.

`crates/engine/src/decimal.rs` has it as `DIV_PRECISION_INCREMENT`, one constant that `AVERAGE_EXTRA_SCALE` is defined from, so there is one place to read the session from once there is a session to read. `crates/engine/src/expr.rs::arithmetic` takes no `Context`, which is the plumbing this needs.

Refusing the SET is the honest half of the gap and is why this is not urgent: nothing silently gets the wrong answer, a client that asks is told the variable does not exist.
