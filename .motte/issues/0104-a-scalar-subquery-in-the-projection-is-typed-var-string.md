---
id: 104
title: A scalar subquery in the projection is typed VAR_STRING
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T14:48:58Z
updated: 2026-08-25T14:48:58Z
---

## Description

Found by the new corpus group in #0058.

    SELECT (SELECT COUNT(*) FROM T)      mysql LONGLONG   engine VAR_STRING
    SELECT (SELECT MAX(t.sid) FROM T t)  mysql LONG       engine VAR_STRING

The binary protocol then sends the string "1" where MySQL sends the number 1, so a prepared-statement client reads a different type than it would from a real server -- which is the protocol this engine exists to serve.

crates/engine/src/computed_column_type has no Expr::Subquery arm and cannot get one without executor access: the type is the type of the subquery's own projection, which means describing the inner statement from inside the describe of the outer one.

The two cases found this way were rewritten in the corpus -- compared rather than selected -- so the baseline stays empty, with a comment saying why. Fixing this means those two can be selected directly.
