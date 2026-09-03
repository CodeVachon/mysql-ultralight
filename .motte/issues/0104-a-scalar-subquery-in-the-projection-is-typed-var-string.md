---
id: 104
title: A scalar subquery in the projection is typed VAR_STRING
state: In Progress
parent: 9
labels: [compat]
created: 2026-08-25T14:48:58Z
updated: 2026-09-03T05:25:45Z
---

## Description

Found by the new corpus group in #0058.

    SELECT (SELECT COUNT(*) FROM T)      mysql LONGLONG   engine VAR_STRING
    SELECT (SELECT MAX(t.sid) FROM T t)  mysql LONG       engine VAR_STRING

The binary protocol then sends the string "1" where MySQL sends the number 1, so a prepared-statement client reads a different type than it would from a real server -- which is the protocol this engine exists to serve.

crates/engine/src/computed_column_type has no Expr::Subquery arm and cannot get one without executor access: the type is the type of the subquery's own projection, which means describing the inner statement from inside the describe of the outer one.

The two cases found this way were rewritten in the corpus -- compared rather than selected -- so the baseline stays empty, with a comment saying why. Fixing this means those two can be selected directly.

## Notes

### 2026-08-26T09:44:53Z — Christopher Vachon (user)

Two measurements from #0073, which hit this from the other side and confirmed the cause is exactly what this issue names -- computed_column_type has no Expr::Subquery arm, and the scalar path reaches query_with_outer directly, so neither the query() materialisation boundary nor from_column_type is involved.

    SELECT (SELECT NULL)                          mysql MYSQL_TYPE_NULL              engine VAR_STRING
    SELECT (SELECT NULL UNION SELECT NULL)        mysql MYSQL_TYPE_STRING binary(0)  engine VAR_STRING
    SELECT (SELECT NULL UNION SELECT NULL LIMIT 1)  mysql STRING                     engine VAR_STRING
    SELECT (SELECT c FROM (SELECT NULL AS c) d)     mysql STRING                     engine VAR_STRING

MySQL reports the inner query's column type, materialised or not, consistently across text, prepare and execute. So this is not only about a scalar subquery over a number -- it inherits whatever the inner query's own column type is, including the materialised binary(0) that c06c2b7 introduced.
