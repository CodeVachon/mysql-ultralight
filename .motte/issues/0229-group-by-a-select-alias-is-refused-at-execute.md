---
id: 229
title: GROUP BY a select alias is refused at execute
state: Done
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T04:21:03Z
updated: 2026-09-04T08:14:42Z
---

## Description

Found while implementing #0100.

    SELECT k AS a FROM Cl GROUP BY a
    mysql   answers two rows
    engine  1054 Unknown column 'a' in 'group statement'

The per-row key evaluation does not substitute the select alias, unlike HAVING which does. #0100's resolve-time check admits aliases so it does not make this worse, and describe.rs asserts the check does not refuse -- but the execute-time bug stands.

Priority 2: GROUP BY appears 82 times in the target application, and aliasing a grouped expression is ordinary SQL.

## Notes

### 2026-09-04T08:14:41Z — Christopher Vachon (user)

Landed in db8294a, gate green, differential 4986/0.

THE PRECEDENCE IS NOT THE OBVIOUS ONE. When a GROUP BY alias shadows a real column, THE COLUMN WINS -- SELECT k AS b FROM Cl GROUP BY b groups on the column and is 1055 for the ungrouped k. That is the OPPOSITE of ORDER BY, which prefers the select list. Yet the select list is still SEARCHED, because SELECT k AS b, s AS b ... GROUP BY b is 1052 ambiguous rather than 1055.

And the search compares RESOLVED COLUMNS, not expressions:
    k AS a, b AS a       1052 (two different columns)
    k AS a, k AS a       answers (same column twice)
    Cl.k AS a, k AS a    answers (one column, two spellings)
    k AS a, 2 AS a       groups on 2 (a field loses to a non-field)

An aggregate or window alias is 1056, and so is a POSITION naming one, reported by the item's output name.

Review caught a regression this change introduced: dropping #0100's alias scope from the name check turned an empty-table 1055 into a 1054.
