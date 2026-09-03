---
id: 104
title: A scalar subquery in the projection is typed VAR_STRING
state: Done
parent: 9
labels: [compat]
created: 2026-08-25T14:48:58Z
updated: 2026-09-03T08:09:18Z
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

### 2026-09-03T08:09:18Z — Christopher Vachon (user)

Done in 1333099 (with #0165).

Approach: describe the inner query eagerly at the moment the outer projection is typed. computed_column_type and the four walkers beside it (never_null, unsigned_answer, binary_answer, temporal_precision) each take a Copy struct holding the engine and the session; the Expr::Subquery arm runs the inner query through a new query_outer wrapped in shaping and probing.

Why not the alternatives: carrying the type after a first execution is unreachable, because describe reads a stored table as empty and the subquery is never evaluated for a table-backed outer query -- the zero-rows pattern from the typing side. The borrow does exist; only plan_expr lacked a session and took one. Going through the query boundary rather than query_with_outer is what keeps (SELECT NULL UNION SELECT NULL) at binary(0), which c06c2b7 established.

Costs, measured rather than estimated: four walkers ask about the same node, so a thread-local memo scoped to one execute/describe collapses the repeats -- mutating it to always miss keeps every test green and takes the engine suite from 0.06s to 20.10s, which is what proves it a cache. Recursion is bounded at depth 8, falling back to VAR_STRING; the guard only bites for table-backed nests, because a FROM-less chain evaluates innermost-first and types at depth 1-2.

Two bugs the work exposed, both fixed. expr::probing set its flag false on exit rather than restoring the previous value, so once a probe nested inside a probe everything after the first subquery in a prepared statement ran UNPROBED -- SELECT (SELECT 1), GET_LOCK('l',0) would have taken the lock at prepare time. And always_one_row ignored the set operator, so (SELECT 1 EXCEPT SELECT 1) was described NOT_NULL and answers NULL; 8.3 is NOT_NULL for UNION and nullable for EXCEPT/INTERSECT.

Also measured: the type matters only in the projection -- a WHERE, an ORDER BY and a SET @x never report it.

Divergences left deliberately: a temporal through a scalar subquery is characters on 8.3 and bytes here (the same quirk MIN(tm) and a DATETIME(3) through a view already decline; marking it bytes would turn a DATE inside a derived table into varbinary); (SELECT 1 LIMIT n) under-reports NOT_NULL, because over-reporting would be NOT_NULL over a NULL; (SELECT NOW(3)) + 0 loses its precision, which a derived table already does identically; and 1241 for a multi-column subquery is still not raised, because the walkers return a type and cannot report an error.

8 mutations, each run twice -- when written and against the final tree. One control passed and is reported as such: mutating the memo to always miss changes no answer.

Corpus 3072 -> 3208 across both issues, baseline empty, both gates verified independently on the merged tree.
