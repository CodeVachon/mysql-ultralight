---
id: 92
title: BETWEEN is not three-valued
state: Todo
parent: 9
labels: [executor]
created: 2026-08-25T10:59:33Z
updated: 2026-08-25T10:59:33Z
---

## Description

Found during #0056 while cross-checking generated statements against a real 8.3, 15 divergences all of one class.

BETWEEN is defined as two comparisons joined by AND, so it short-circuits the way AND does and a NULL bound does not always make it NULL:

    SELECT 3 BETWEEN 4 AND NULL   mysql 0      engine NULL   -- 3 >= 4 is false, so AND settles it
    SELECT 5 BETWEEN NULL AND 4   mysql 0      engine NULL   -- 5 <= 4 is false
    SELECT 3 BETWEEN NULL AND 4   mysql NULL   engine NULL   -- genuinely unknown

crates/engine/src/expr.rs's Expr::Between arm answers NULL whenever either comparison is unknown, which is right only when the other one is true.

A wrong answer rather than a refusal. Untouched by #0056, which only changed how the construct is grouped, not how it is evaluated.
