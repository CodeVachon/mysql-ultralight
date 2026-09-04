---
id: 241
title: An ambiguous unqualified name in an ON is not detected
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-04T08:10:40Z
updated: 2026-09-04T08:10:40Z
---

## Description

Found while implementing #0230, pre-existing.

    SELECT ... FROM Cr JOIN Cr c3 ON c3.id = id
    mysql   1052 Column 'id' in on clause is ambiguous
    engine  takes the first match and answers

Context::lookup documents taking the first match rather than detecting ambiguity. #0229 taught the GROUP BY path to raise 1052 for an ambiguous alias, so the error and the shape of the check now exist -- this is the ON path's version of the same rule.

A wrong answer rather than a wrong code: the engine silently picks a column where MySQL refuses to guess.
