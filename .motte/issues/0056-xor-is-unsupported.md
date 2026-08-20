---
id: 56
title: XOR is unsupported
state: Todo
parent: 9
labels: [executor]
created: 2026-08-20T20:16:48Z
updated: 2026-08-20T20:16:48Z
---

## Description

SELECT 1 XOR 0 fails with 1235 "does not yet support operator `XOR`". MySQL: 1 XOR 0 is 1, 0 XOR 0 and 1 XOR 1 are 0, and NULL XOR anything is NULL — the same three-valued shape as AND and OR, which crates/engine/src/expr.rs already implements beside it. Found while measuring column names for #0043. A clear error today, not a wrong answer.
