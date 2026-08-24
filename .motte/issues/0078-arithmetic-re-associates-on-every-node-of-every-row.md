---
id: 78
title: Arithmetic re-associates on every node of every row
state: Done
parent: 9
labels: [executor]
created: 2026-08-24T17:36:11Z
updated: 2026-08-24T19:24:53Z
---

## Description

Regression introduced by #0052 in 846c925, found by review during #0053.

precedence() returns a non-zero level for + - * / % DIV, so reassociated() no longer short-circuits the way tightness() let it. Every arithmetic node now deep-clones its operands, flattens the chain and rebuilds it by precedence climbing -- per node, per row -- where before the common case returned immediately.

Measured by the reviewer: a 12-term chain over 20k rows went 9.1 ms -> 1294 ms (142x); at 24 terms, 15 ms -> 5913 ms (391x).

The rebuild is correct and has to stay; what is missing is the early exit. A chain whose operators are already in non-increasing precedence order left to right is the tree sqlparser would have built anyway, so it can be detected and returned untouched without allocating. The check is cheap and the common case -- a + b, a * b + c -- hits it.

This engine's whole premise is start-up and per-statement latency, so this is a defect rather than a tuning note.

## Notes

### 2026-08-24T19:24:53Z — Christopher Vachon (user)

Done in f2c6a1a. 12 terms over 20k rows: 253 ms -> 7.3 ms. 24 terms: 885 ms -> 15.0 ms. Two terms, a + b: 9.5 ms -> 1.6 ms, so the fix is below the pre-#0052 cost rather than merely back at it.

This issue's proposed fix was wrong and the implementation says so: checking that the operators are non-increasing left to right is not sufficient, because 2 ^ 3 * 4 is non-increasing under MySQL's order and sqlparser still builds 2 ^ (3 * 4). The recognition is on the shape of the tree -- a node is already MySQL's tree when its left operand is no looser than itself and its right operand strictly tighter, recursively, which is the unique precedence-climbing tree for that token sequence. The root of a correct chain is the last of its loosest operators, which is why a local test settles the whole answer.

Three things take the slow path rather than being reasoned about: an operator with no measured level (the = inside doc -> ('$.b' = 6)), the constructs take_leftmost walks, and a ~ holding a binary operator.

An early exit is invisible except in what it costs, so the test counts allocations through a thread-local global allocator and requires zero for the shapes it claims to spare. Getting to zero meant answering the ~ case in grouped without calling reassociated_unary, which clones.

Correctness held: 12000 grouping expressions and 5998 value expressions against a reference precedence-climbing evaluator, 0 divergences, identical to HEAD. Review independently fuzzed 200k expressions asserting that a fast-path accept implies the slow path would have returned None -- 0 false accepts, and no findings. Differential 312 statements, baseline empty.

Benchmark and fuzz were throwaways, deliberately not kept: a wall-clock threshold in cargo test --workspace would be a flaky gate. The in-repo proof is the allocation count.
