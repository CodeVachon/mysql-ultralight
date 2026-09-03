---
id: 179
title: Dropping a column leaves its indexes behind
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T12:29:35Z
updated: 2026-09-03T14:20:32Z
---

## Description

Found while implementing #0133, pre-existing.

    CREATE TABLE t (a INT, INDEX ix (a));
    ALTER TABLE t DROP COLUMN a

MySQL removes KEY ix with the column. The engine keeps it, so information_schema.statistics lists an index over a column that no longer exists.

Inert for enforcement -- check_unique skips indexes whose columns do not resolve (catalog.rs:192) -- so this is a visibility divergence rather than a wrong answer to a write. It still means a schema read here does not describe a schema MySQL could hold.
