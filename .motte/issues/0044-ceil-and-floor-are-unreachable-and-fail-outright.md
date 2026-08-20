---
id: 44
title: CEIL and FLOOR are unreachable and fail outright
state: Todo
parent: 9
labels: [executor]
created: 2026-08-20T14:48:41Z
updated: 2026-08-20T14:48:41Z
---

## Description

Found while fixing #0036. SELECT CEIL(1.5) is refused with 1235 'does not yet support expression CEIL(1.5)' - note *expression*, not function. sqlparser gives CEIL and FLOOR dedicated AST nodes rather than parsing them as an ordinary function call, and crates/engine/src/expr.rs handles neither, so the "ceil" | "ceiling" | "floor" arms in functions.rs are unreachable from SQL.

Code that reads as implemented but cannot be called is worse than an obvious gap: the function list says the engine supports CEIL.

The fix is arms in expr.rs that route the dedicated nodes to the existing implementations, and a test that actually goes through SQL rather than calling functions::call directly.
