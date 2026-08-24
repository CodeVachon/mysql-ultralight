---
id: 52
title: The bitwise operators are unsupported
state: Done
parent: 9
labels: [executor]
created: 2026-08-20T19:24:50Z
updated: 2026-08-24T17:04:41Z
---

## Description

Split out of #0042. SELECT 7 ^ 3 fails with 1235 "does not yet support operator `^`". The same applies to & | << >> and the unary ~.

MySQL evaluates these over unsigned 64-bit integers and reports the result as BIGINT UNSIGNED, which is why they are their own piece of work rather than more arithmetic: -1 & 1 is 1 and SELECT ~0 is 18446744073709551615, so the engine's signed i64 Value cannot carry the answers without care.

Note for whoever takes this: crates/engine/src/expr.rs::reassociated_div declines to rotate past BitwiseXor, because ^ is the one operator binding tighter than DIV. That arm is currently unreachable in practice — the evaluator rejects ^ before the shape matters — and becomes live when this lands.

Not built because the conformance corpus uses none of them (every ^ in it is a regex character class inside a string literal), and the current behaviour is a clear error rather than a wrong answer.

## Notes

### 2026-08-20T19:38:59Z — Christopher Vachon (user)

Also fix sqlparser's precedence for ^. It gives Caret 22, below PlusMinus 30 and MulDivMod 40, so 2 ^ 3 + 1 parses as 2 ^ (3 + 1) where MySQL means (2 ^ 3) + 1 — wrong independently of DIV. crates/engine/src/expr.rs::divide_leftmost stops at a BitwiseXor for this reason and says so; that arm is correct for a bare a DIV b ^ c and wrong for a DIV b ^ c + d until the precedence is fixed.

### 2026-08-24T17:04:41Z — Christopher Vachon (user)

Done in 846c925. Evaluation composes the whole chain over u64 and range-checks only the value the chain hands out; checking per operator refused 1354 of 6000 expressions it can answer exactly, 1 + 3 & ~5 among them.

Precedence could not be fixed a pair at a time. Rotating one pair reads the left operand before its own correction has changed its root, which a fuzz against a reference precedence-climbing evaluator caught answering 2 ^ 3 + 2 ^ 3 as 0 where MySQL says 2 -- about 150 such shapes in 6000 expressions, every one silent. The chain is now flattened and rebuilt by precedence climbing under MySQL's measured order: ^ > * / DIV % > + - > << >> > & > |. 32000 generated expressions agree with 8.3.

Measured and encoded while here: || under PIPES_AS_CONCAT binds tighter than everything but ->, which the old tightness table had below ^; a number operand rounds away from zero where a string operand reads its leading integer; shifts are logical with an unreduced unsigned count.

Refused rather than answered, per the never-a-wrong-answer rule: a bitwise value above i64::MAX (SELECT ~0, 1 << 63, -5 & -3) and a bitwise operator over a binary string. See #0077 for the first; the second is because Value::Bytes is what both X'0F' and a VARBINARY column come back as and MySQL reads them oppositely.

Corpus 260 -> 302, baseline still empty. 14 mutations bite; two of them covered for each other until a distinguishing case was added for each.
