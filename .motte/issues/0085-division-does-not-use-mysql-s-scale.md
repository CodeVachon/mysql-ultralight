---
id: 85
title: Division does not use MySQL's scale
state: Todo
parent: 9
labels: [executor]
created: 2026-08-25T00:52:51Z
updated: 2026-08-25T00:52:51Z
---

## Description

Found while measuring for #0054, and broader than that issue.

MySQL gives a division four more decimal places than its left operand -- div_precision_increment, default 4 -- and the engine returns whatever the f64 printed:

    SELECT 1/3        mysql 0.3333               engine 0.3333333333333333
    SELECT d / 1      mysql 20240105.0000        engine 20240105
    SELECT 10/3       mysql 3.3333               engine 3.3333333333333335

#0054 deliberately did not half-fix this. Adding the increment for temporal operands only would have made the gap depend on what was divided, which is worse than a gap that is at least uniform.

#0070 covers the scale rules for + - * over exact operands and names division's rule in passing; this is division specifically, over every operand type, and it is the one arithmetic operator whose scale is wrong even when both operands are plain integers. Whoever takes #0070 should probably take this with it -- crates/engine/src/expr.rs::exact_arithmetic is where both land, and its doc comment already records why division was left out.
