---
id: 236
title: HAVING prefers a select alias where MySQL prefers a grouped column
state: In Progress
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T08:10:22Z
updated: 2026-09-04T10:04:43Z
---

## Description

Found while implementing #0229, which established the same precedence rule for GROUP BY. Measured on 8.3:

    SELECT k AS b, COUNT(*) FROM Cl GROUP BY id, k, b HAVING b > 50
    mysql   3 rows -- b resolves to the COLUMN b (100/200/300)
    engine  0 rows -- b resolves to the ALIAS (= k)

And MySQL FLIPS when the column is not grouped:

    SELECT k AS b, COUNT(*) FROM Cl GROUP BY id HAVING b > 50
    mysql   0 rows -- b resolves to the ALIAS

So the rule appears to be: a GROUPED column beats the alias; otherwise the alias wins. The engine's substitute_aliases substitutes unconditionally, so it always takes the alias.

Same family as #0229 (GROUP BY: the column wins outright) and the ORDER BY rule (the select list wins outright) -- three clauses, three different precedences, and this is the only one that depends on what else is in the GROUP BY.

Priority 2: HAVING appears alongside 82 GROUP BY sites in the target application, and a wrong-rows answer is worse than a wrong error.
