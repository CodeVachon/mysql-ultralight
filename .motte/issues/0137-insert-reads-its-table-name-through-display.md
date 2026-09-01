---
id: 137
title: INSERT reads its table name through Display
state: Done
parent: 9
labels: [executor]
created: 2026-08-31T13:35:08Z
updated: 2026-09-01T07:46:27Z
---

## Description

Found by the Display-pattern audit in #0093, and a wrong answer.

crates/engine/src/exec.rs's INSERT path does `insert.table.to_string()` and then splits the rendering on '.'. Every other statement path uses split_name(&ObjectName), which reads name.0 part by part.

    CREATE TABLE `my.table` (a INT)     engine ok
    SELECT * FROM `my.table`            engine ok
    INSERT INTO `my.table` VALUES (1)   engine 1049 Unknown database 'my'   mysql inserts the row

So the engine disagrees with itself about the same name: it will create and read a table it cannot write to.

The fix is to use split_name like every other path. This is the fourth instance of one class -- #0058 read a qualifier through Display and kept its backticks, #0059 hit it in literal naming, #0093 in ESCAPE, and this. Reading a parsed value back through its rendering loses the difference between what was written and what it means, and it has failed silently every time.

## Notes

### 2026-09-01T07:46:27Z — Christopher Vachon (user)

Done in 294e19c, swept together with #0138 as one class.

INSERT split the rendering of its table name on a dot where every other path reads name.0 part by part, so the engine would create and read a table it could not write to: INSERT INTO `my.table` VALUES (1) was 1049 Unknown database 'my' while CREATE and SELECT on the same name worked. Names now come from a shared name.rs.

This was the fourth instance of the class -- #0058 (a qualifier kept its backticks and refused legal SQL), #0059 (literal naming), #0093 (LIKE's escape taken from the opening quote), and this. The sweep covered all six crates rather than the audited list, and classified the deliberate renderings -- view definitions, CHECK text, generated columns, column naming from source -- as correct and left them alone.

Review found and fixed alongside: a qualified column in an INSERT list (INSERT INTO q (q.a, q.b) is legal on 8.3) was looked up as a column literally named q.a.
