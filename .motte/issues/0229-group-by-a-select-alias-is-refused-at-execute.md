---
id: 229
title: GROUP BY a select alias is refused at execute
state: Todo
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T04:21:03Z
updated: 2026-09-04T04:21:03Z
---

## Description

Found while implementing #0100.

    SELECT k AS a FROM Cl GROUP BY a
    mysql   answers two rows
    engine  1054 Unknown column 'a' in 'group statement'

The per-row key evaluation does not substitute the select alias, unlike HAVING which does. #0100's resolve-time check admits aliases so it does not make this worse, and describe.rs asserts the check does not refuse -- but the execute-time bug stands.

Priority 2: GROUP BY appears 82 times in the target application, and aliasing a grouped expression is ordinary SQL.
