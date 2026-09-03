---
id: 179
title: Dropping a column leaves its indexes behind
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T12:29:35Z
updated: 2026-09-03T19:15:27Z
---

## Description

Found while implementing #0133, pre-existing.

    CREATE TABLE t (a INT, INDEX ix (a));
    ALTER TABLE t DROP COLUMN a

MySQL removes KEY ix with the column. The engine keeps it, so information_schema.statistics lists an index over a column that no longer exists.

Inert for enforcement -- check_unique skips indexes whose columns do not resolve (catalog.rs:192) -- so this is a visibility divergence rather than a wrong answer to a write. It still means a schema read here does not describe a schema MySQL could hold.

## Notes

### 2026-09-03T19:15:26Z — Christopher Vachon (user)

Landed in 666676a, gate green. Differential 4248 statements, 0 divergences.

The measurement that mattered: MySQL NARROWS a multi-column index rather than dropping it. INDEX ix (a,b) minus a is ix (b) with SEQ_IN_INDEX renumbered to 1; a narrowed UNIQUE keeps NON_UNIQUE 0 and marks the survivor UNI; a multi-column PRIMARY KEY narrows and the survivor stays PRI. Only a single-column index goes entirely. A naive 'drop the index with the column' fix would have been wrong on every multi-column key.
