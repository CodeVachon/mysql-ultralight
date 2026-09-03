---
id: 147
title: A window function on the grouped path refuses
state: Todo
parent: 9
labels: [executor]
created: 2026-09-01T03:03:30Z
updated: 2026-09-03T14:02:15Z
---

## Description

Found while implementing #0103, and confirmed pre-existing by stashing that change and re-measuring -- 1235 either way.

    SELECT b, COUNT(*) OVER () c FROM Hv GROUP BY b HAVING b > 1
    mysql   (2,2),(3,2)
    engine  1235

A window function is supported on the ungrouped path and refused on the grouped one, so whether a query can use one depends on whether it also groups.

MySQL evaluates a window after grouping, over the group rows -- the same relationship #0103 established for the ungrouped case, where the window sees the rows the HAVING kept. So the rule is already measured; what is missing is the grouped path calling it.

A refusal rather than a wrong answer. Worth checking PARTITION BY over a grouped column and an aggregate inside the window's own argument (COUNT(*) OVER (ORDER BY SUM(id))) in the same pass, since both are legal on 8.3.

## Notes

### 2026-09-03T14:02:15Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. 8.3 answers (2,2),(3,2); the engine refuses -- though the message is 1235 'does not yet support function count()' rather than anything window-specific, which is worth knowing since the refusal comes from the aggregate, not the OVER clause.
