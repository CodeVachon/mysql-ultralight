---
id: 92
title: BETWEEN is not three-valued
state: Done
parent: 9
labels: [executor]
created: 2026-08-25T10:59:33Z
updated: 2026-09-03T02:41:33Z
---

## Description

Found during #0056 while cross-checking generated statements against a real 8.3, 15 divergences all of one class.

BETWEEN is defined as two comparisons joined by AND, so it short-circuits the way AND does and a NULL bound does not always make it NULL:

    SELECT 3 BETWEEN 4 AND NULL   mysql 0      engine NULL   -- 3 >= 4 is false, so AND settles it
    SELECT 5 BETWEEN NULL AND 4   mysql 0      engine NULL   -- 5 <= 4 is false
    SELECT 3 BETWEEN NULL AND 4   mysql NULL   engine NULL   -- genuinely unknown

crates/engine/src/expr.rs's Expr::Between arm answers NULL whenever either comparison is unknown, which is right only when the other one is true.

A wrong answer rather than a refusal. Untouched by #0056, which only changed how the construct is grouped, not how it is evaluated.

## Notes

### 2026-09-03T02:41:33Z — Christopher Vachon (user)

Done in 6d43d59, in an isolated worktree.

BETWEEN is two comparisons joined by AND and short-circuits the way AND does: 3 BETWEEN 4 AND NULL is 0 because three is not four or more and the missing bound cannot change that; 5 BETWEEN NULL AND 4 is 0 for the mirror reason; only 3 BETWEEN NULL AND 4 is genuinely unknown.

Invisible in a WHERE clause, where NULL and 0 both reject the row -- which is why the corpus had never asked. It shows in a projection, inside CASE or IF, in an aggregate over it, and under IS NULL on the result. The corpus cases ask it where it is visible.

NOT BETWEEN was measured separately rather than negated: where the answer is unknown it stays unknown, so it is not the complement of BETWEEN.

Two gaps recorded in the arm rather than half-fixed. MySQL evaluates both bounds even where the first settles the answer, so a bound that would raise still raises -- reproducing that means evaluating for an effect this engine does not otherwise have. And review found the comment's premise wrong: BETWEEN is only 'a >= lo AND a <= hi' where the three operands agree on type, because MySQL aggregates ONE comparison type across all three. '2' BETWEEN 1 AND '10' is 1 on 8.3 and 0 here, while the pair '2' <= '10' is 0 on both. Filed as #0156, deliberately without a corpus case, since a baseline line without an issue is what the README forbids.

Corpus 2568 -> 2596, baseline empty, both gates verified independently on the merged tree.

Note the gate needed three attempts: vitest failed twice on 5-second timeouts under three concurrent agents, and passes in 350 ms standalone. Filed as #0157.
