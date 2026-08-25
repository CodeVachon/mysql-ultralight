---
id: 56
title: XOR is unsupported
state: Done
parent: 9
labels: [executor]
created: 2026-08-20T20:16:48Z
updated: 2026-08-25T11:26:06Z
---

## Description

SELECT 1 XOR 0 fails with 1235 "does not yet support operator `XOR`". MySQL: 1 XOR 0 is 1, 0 XOR 0 and 1 XOR 1 are 0, and NULL XOR anything is NULL — the same three-valued shape as AND and OR, which crates/engine/src/expr.rs already implements beside it. Found while measuring column names for #0043. A clear error today, not a wrong answer.

## Notes

### 2026-08-25T11:26:05Z — Christopher Vachon (user)

Done in 216ff14. Far larger than the issue implied: the evaluation was one arm, and the grouping was the work.

sqlparser puts XOR at 24, between + and &, so it disagreed with MySQL about every comparison, every bitwise operator, AND, and every postfix construct. A chain stops at an operator with no measured level, so XOR could not be moved past what was absent -- AND, OR, || where it means OR, the comparisons and the IN/BETWEEN/LIKE/REGEXP family all had to be added, and everything above renumbered twice.

IS NULL, IN, BETWEEN and LIKE are not BinaryOp nodes, so no correction had ever seen them. They needed their own pass: un-swallow the trailing operand, then sink the construct past its leading chain.

NOT needed a third correction, not a reuse of the second, and review caught its absence turning a clean 1235 into a wrong answer -- NOT 0 XOR 1 AND 0 answered 0 against MySQL's 1. ~ binds to the operand beside it so the walk pulls everything out; NOT (1 + 2) is already MySQL's grouping and pulling the + out answers 1. So NOT sinks leftward, recursively, and its operand is corrected first -- NOT 2 || NULL IN (1) arrives with the IN already swallowed, which a fuzz against real MySQL found rather than reasoning.

Verified by generating from MySQL's own grammar and comparing against 8.3 rather than a hand-written model: 4000 statements 0 divergences, and 0 again under PIPES_AS_CONCAT. An earlier reviewer's 40000-expression run was clean once NOT was excluded, which is what identified NOT as the only hole.

Two pre-existing wrong answers fell out of the table: 1 || 1 AND 0 was 0 against MySQL's 1, because sqlparser reads || at *'s precedence whichever operator it is, and 1 || 1 was described VAR_STRING and nullable; and 'a' LIKE 'a' = 1 was 0 against 1.

Metadata measured identical to AND throughout: LONGLONG, length 1, NOT_NULL only when neither operand is nullable, never UNSIGNED even over an INT UNSIGNED.

Two gaps measured and filed rather than half-fixed: #0091 (BETWEEN's high takes a predicate, needing a correction that pushes inward) and #0092 (BETWEEN is not three-valued -- the fuzz's residual 93 divergences were all of that one class).

34 mutations. Two are inert and are now measurably so rather than by luck: MySQL's grammar forbids a bit_expr holding a predicate, so the family that types differently can never reach the root, and the 1064s that prove it are in the comment. Two review rounds also removed two false-pass fuzz files that reported divergences and still exited ok. Corpus 870 -> 968, baseline empty.
