---
id: 137
title: INSERT reads its table name through Display
state: In Progress
parent: 9
labels: [executor]
created: 2026-08-31T13:35:08Z
updated: 2026-09-01T03:03:41Z
---

## Description

Found by the Display-pattern audit in #0093, and a wrong answer.

crates/engine/src/exec.rs's INSERT path does `insert.table.to_string()` and then splits the rendering on '.'. Every other statement path uses split_name(&ObjectName), which reads name.0 part by part.

    CREATE TABLE `my.table` (a INT)     engine ok
    SELECT * FROM `my.table`            engine ok
    INSERT INTO `my.table` VALUES (1)   engine 1049 Unknown database 'my'   mysql inserts the row

So the engine disagrees with itself about the same name: it will create and read a table it cannot write to.

The fix is to use split_name like every other path. This is the fourth instance of one class -- #0058 read a qualifier through Display and kept its backticks, #0059 hit it in literal naming, #0093 in ESCAPE, and this. Reading a parsed value back through its rendering loses the difference between what was written and what it means, and it has failed silently every time.
