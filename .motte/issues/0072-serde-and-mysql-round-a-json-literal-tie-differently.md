---
id: 72
title: serde and MySQL round a JSON literal tie differently
state: In Progress
parent: 9
labels: [compat]
created: 2026-08-21T18:23:38Z
updated: 2026-08-26T03:49:17Z
---

## Description

SELECT CAST('[2000000000000000.25]' AS JSON)
    mysql   [2e15]
    engine  [2000000000000000.3]

Found while closing #0046. This is not the number *formatting* that issue was about — crates/engine/src/json.rs now reproduces my_gcvt's rule exactly, and every value the two parsers agree on is written identically. The disagreement is one step earlier: serde_json and MySQL parse that literal to different doubles, because 17 significant digits at that magnitude land on a tie and the two break it the other way. Rust's own compiler-parsed literal agrees with serde.

Bounded to inputs written with more significant digits than a double carries, at a magnitude where the ulp makes the extra digits a tie — roughly [1e15, 4.5e15) for a two-decimal input. Nothing in the corpus or the target codebase is near it.
