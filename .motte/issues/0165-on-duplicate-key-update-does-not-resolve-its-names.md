---
id: 165
title: ON DUPLICATE KEY UPDATE does not resolve its names
state: In Progress
parent: 9
labels: [executor]
created: 2026-09-03T04:51:36Z
updated: 2026-09-03T05:25:45Z
---

## Description

Found while implementing #0102, and the fifth entry point with the identical hole.

    INSERT INTO t (k) VALUES (1) ON DUPLICATE KEY UPDATE k = a.b
    mysql   1054 Unknown column 'a.b' in 'field list'
    engine  inserts the row and reports success

#0102 closed the hole in update(), update_joined(), delete() and delete_joined(). This clause was deliberately left out because its scope is different: it resolves VALUES(x) and the insert's own column list as well as the target table, and #0102's own review found that getting a scope wrong turns a legal statement into a refusal (SET col = DEFAULT parsed as a column named DEFAULT and was refused).

So this needs its scope measured before the guard is added, not copied from the WHERE path. Establish what VALUES(x) resolves against, whether the insert's column list is in scope for the update expressions, and whether an alias introduced by INSERT ... SELECT is visible there.

Recorded in resolve_dml_where's doc comment with the measurement.
