---
id: 44
title: CEIL and FLOOR are unreachable and fail outright
state: Done
parent: 9
labels: [executor]
created: 2026-08-20T14:48:41Z
updated: 2026-08-21T02:28:39Z
---

## Description

Found while fixing #0036. SELECT CEIL(1.5) is refused with 1235 'does not yet support expression CEIL(1.5)' - note *expression*, not function. sqlparser gives CEIL and FLOOR dedicated AST nodes rather than parsing them as an ordinary function call, and crates/engine/src/expr.rs handles neither, so the "ceil" | "ceiling" | "floor" arms in functions.rs are unreachable from SQL.

Code that reads as implemented but cannot be called is worse than an obvious gap: the function list says the engine supports CEIL.

The fix is arms in expr.rs that route the dedicated nodes to the existing implementations, and a test that actually goes through SQL rather than calling functions::call directly.

## Notes

### 2026-08-21T02:28:39Z — Christopher Vachon (user)

CEIL and FLOOR routed to the existing implementations, tested through SQL. Review then found that every walk over an expression had the same shape of hole — references_column, contains_aggregate, fold_aggregates and the ONLY_FULL_GROUP_BY check each enumerated variants and defaulted to false, so TRIM(x), POSITION and LIKE were invisible too. Replaced two of them with one shared child list. Also fixed a process abort on any non-ASCII literal. Split out #0059 and #0060.
