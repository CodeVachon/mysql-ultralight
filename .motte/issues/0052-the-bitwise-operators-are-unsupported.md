---
id: 52
title: The bitwise operators are unsupported
state: Todo
parent: 9
labels: [executor]
created: 2026-08-20T19:24:50Z
updated: 2026-08-20T19:38:59Z
---

## Description

Split out of #0042. SELECT 7 ^ 3 fails with 1235 "does not yet support operator `^`". The same applies to & | << >> and the unary ~.

MySQL evaluates these over unsigned 64-bit integers and reports the result as BIGINT UNSIGNED, which is why they are their own piece of work rather than more arithmetic: -1 & 1 is 1 and SELECT ~0 is 18446744073709551615, so the engine's signed i64 Value cannot carry the answers without care.

Note for whoever takes this: crates/engine/src/expr.rs::reassociated_div declines to rotate past BitwiseXor, because ^ is the one operator binding tighter than DIV. That arm is currently unreachable in practice — the evaluator rejects ^ before the shape matters — and becomes live when this lands.

Not built because the conformance corpus uses none of them (every ^ in it is a regex character class inside a string literal), and the current behaviour is a clear error rather than a wrong answer.

## Notes

### 2026-08-20T19:38:59Z — Christopher Vachon (user)

Also fix sqlparser's precedence for ^. It gives Caret 22, below PlusMinus 30 and MulDivMod 40, so 2 ^ 3 + 1 parses as 2 ^ (3 + 1) where MySQL means (2 ^ 3) + 1 — wrong independently of DIV. crates/engine/src/expr.rs::divide_leftmost stops at a BitwiseXor for this reason and says so; that arm is correct for a bare a DIV b ^ c and wrong for a DIV b ^ c + d until the precedence is fixed.
