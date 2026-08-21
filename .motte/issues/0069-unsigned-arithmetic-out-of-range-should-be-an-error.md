---
id: 69
title: Unsigned arithmetic out of range should be an error
state: Todo
parent: 9
labels: [executor]
created: 2026-08-21T14:04:47Z
updated: 2026-08-21T14:04:47Z
---

## Description

MySQL refuses rather than answering when arithmetic on an UNSIGNED operand leaves its range:

    SELECT iu - 100 FROM Q   -- iu INT UNSIGNED holding 7
    mysql   ERROR 1690 (22003): BIGINT UNSIGNED value is out of range in '(`Q`.`iu` - 100)'
    engine  -93

Found by review during #0064. The engine holds an i64 and answers, which is why #0064 leaves the UNSIGNED flag off a subtraction: describing the column unsigned while sending -93 would have the text protocol say -93 and the binary one hand the same bytes to a client reading 18446744073709551523. Under-reporting the flag keeps the engine's own two protocols agreeing; it does not make either of them match MySQL.

The same applies to a sum or a product that wraps, which crates/engine/src/expr.rs does deliberately with wrapping_add and wrapping_mul.

Doing this properly needs the evaluator to know an operand is unsigned, which it does not: arithmetic sees only Values and the sign lives on the column. The same shape as #0054 and #0068. Once it lands, `Minus` belongs back in the list in `unsigned_answer`.
