---
id: 158
title: A FROM-less SELECT ignores its WHERE clause
state: Done
parent: 9
labels: [executor]
created: 2026-09-03T02:43:34Z
updated: 2026-09-03T04:09:34Z
---

## Description

Found by review during #0116, pre-existing and unrelated to that issue.

    SELECT 1 WHERE 0        mysql returns no rows   engine returns one row
    SELECT 1 WHERE 1 = 0    same

The FROM-less SELECT path evaluates its projection and never consults the WHERE clause at all.

A plain wrong answer in one of the simplest statements SQL has, and one a client can easily generate -- SELECT <expr> WHERE <predicate> with no table is a normal shape for a probe or a guard query.

It also masks other refusals: SELECT 1 WHERE 1 IN (SELECT SIGN(1,2)) answers a row rather than raising, and the same statement with a malformed LIKE ... ESCAPE misbehaves identically, because neither the predicate nor anything inside it is ever reached.

Related in shape to #0009's zero-rows note but the opposite direction: there a check never ran because no row was evaluated; here a clause is never evaluated at all, so a row is returned that should not be.

Check GROUP BY, HAVING and ORDER BY on the same path in one pass -- #0103 fixed the FROM-less HAVING, so the projection path clearly reaches some clauses and not others, and it is worth establishing which.

## Notes

### 2026-09-03T04:09:34Z — Christopher Vachon (user)

Done in 5a075ba, in an isolated worktree. The deliverable was the whole FROM-less path, not the one clause, and that was right: GROUP BY was never reached either.

Clause table, before -> after: the field list was evaluated but only resolved BY evaluating (ordering was luck, now resolved first); WHERE was never reached at all; GROUP BY was never reached at all; HAVING was already correct (#0103) and now folds over what the WHERE left; ORDER BY was already correct (#0090/#0144); LIMIT/OFFSET already applied; DISTINCT is vacuous on one row.

Symptoms before: SELECT 1 WHERE 0 answered a row; WHERE zz=1 answered where 8.3 says 1054; WHERE 1 IN (SELECT SIGN(1,2)) answered where 8.3 says 1582; WHERE 'x' LIKE 'a' ESCAPE '!!' answered where 8.3 says 1210. GROUP BY 2 and GROUP BY zz likewise.

The subtlety a naive filter gets wrong: a rejected implicit row is NOT no result. SELECT COUNT(*) WHERE 0 is one row holding 0 on 8.3 (SUM/MAX/MIN/AVG/GROUP_CONCAT all NULL beside it), where SELECT 1 WHERE 0 is none; an aggregate written only in the HAVING counts too; and a GROUP BY takes the row away again.

Resolution order measured and matched: field list, WHERE, GROUP BY, HAVING, ORDER BY -- confirmed with statements carrying two bad clauses. A runtime error in the WHERE does preempt GROUP BY resolution on 8.3 (1690, not 1054), so evaluate-then-resolve is correct here.

The mutation that neuters the resolve-time refusals PASSED at first, because this path always has one implicit row so the evaluator reaches the predicate anyway. Only an untaken CASE branch and a short-circuited AND separate the two readings; the test now uses both.

Three of the review's five findings were measured false rather than fixed: an aggregate's argument is not evaluated over an empty group on 8.3 either (SUM(9223372036854775807 + 1) WHERE 0 is NULL there), ORDER BY COUNT(*) is 3029 rather than a row, and the WHERE-before-GROUP-BY order was already right. The residue of the first is only a function 8.3 resolves by name -- recorded with the measurements plus a test pinning the pair.

15 mutations. Corpus 2758 -> 2866, baseline empty, both gates verified independently on the merged tree; the merge kept both this group and #0132's, which landed at the same insertion point.

Filed: #0163 (FROM DUAL is 1146 here and a one-row table on 8.3), #0164 (check.sh exits non-zero on its own skip path).
