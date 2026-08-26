---
id: 85
title: Division does not use MySQL's scale
state: Done
parent: 9
labels: [executor]
created: 2026-08-25T00:52:51Z
updated: 2026-08-26T00:52:54Z
---

## Description

Found while measuring for #0054, and broader than that issue.

MySQL gives a division four more decimal places than its left operand -- div_precision_increment, default 4 -- and the engine returns whatever the f64 printed:

    SELECT 1/3        mysql 0.3333               engine 0.3333333333333333
    SELECT d / 1      mysql 20240105.0000        engine 20240105
    SELECT 10/3       mysql 3.3333               engine 3.3333333333333335

#0054 deliberately did not half-fix this. Adding the increment for temporal operands only would have made the gap depend on what was divided, which is worse than a gap that is at least uniform.

#0070 covers the scale rules for + - * over exact operands and names division's rule in passing; this is division specifically, over every operand type, and it is the one arithmetic operator whose scale is wrong even when both operands are plain integers. Whoever takes #0070 should probably take this with it -- crates/engine/src/expr.rs::exact_arithmetic is where both land, and its doc comment already records why division was left out.

## Notes

### 2026-08-26T00:52:54Z — Christopher Vachon (user)

Closed with #0070 in 7a5f988, for the reason recorded here: both land in exact_arithmetic, and fixing + - * without / would have made the gap depend on what was divided.

SELECT 1/3 is 0.3333 and d / 1 over a temporal is 20240105.0000. div_precision_increment measured as default 4, session-settable, and affecting / only -- DIV and MOD are unchanged at any setting. Division computes to a nine-digit-word boundary by truncating and only then rounds, so 1.00001/3 is 0.333336666.

One residue is tracked separately as #0118: MySQL's quotient value carries more fractional digits than its column declares, so JSON_ARRAY(1/3) is [0.333333333] against a decimal(5,4) column. This engine rounds to the displayed scale, which makes every printed quotient right and is all it can do until a computed column carries a declared scale (#0111).
