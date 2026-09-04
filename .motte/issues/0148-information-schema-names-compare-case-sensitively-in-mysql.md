---
id: 148
title: information_schema names compare case-sensitively in MySQL
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-01T06:09:08Z
updated: 2026-09-04T02:47:23Z
---

## Description

Found while adding corpus cases during #0137/#0138, and unrelated to them.

information_schema.TABLE_NAME is utf8mb3_bin on 8.3, so both ordering and equality over it are case-sensitive there and case-insensitive here:

    ORDER BY TABLE_NAME        mysql sorts Bee before apple   engine sorts apple first
    WHERE TABLE_NAME = 'bee'   mysql matches nothing          engine matches Bee

Two wrong answers -- a wrong order and a wrong match -- reachable by any client that introspects the catalog, which is what a migration tool does before it emits DDL. Prisma introspects.

Fixing it needs collation-aware comparison, which this engine does not have: crates/engine/src/value.rs collates everything one way. Same root as #0129 (a column's own character set is not honoured) and #0134 (LIKE is case-sensitive where MySQL folds) -- all three are the same missing mechanism seen from different sides, and #0134 is the mirror image of this one.

The corpus tables added in #0137/#0138 are deliberately lower-case so this does not surface there, with the reason written above them.

## Notes

### 2026-09-03T14:04:20Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. ORDER BY TABLE_NAME puts Bee before apple on 8.3 and after it here; WHERE TABLE_NAME='bee' matches nothing there and matches Bee here.

### 2026-09-04T02:47:22Z — Christopher Vachon (user)

Landed in a67339c, gate green, differential 4612/0. NOT ONE RULE: information_schema's columns carry THREE collations on 8.3 -- utf8mb3_bin for schema/table/type names, utf8mb3_tolower_ci for column/index/constraint names, utf8mb3_general_ci for the rest. So = 'bee' finds nothing against Bee while COLUMN_NAME = 'ALPHA' matches, and only the first group is case-sensitive.

It is a COLLATION and not a case-sensitivity flag, proved by TABLE_NAME = 'Bee ' answering one row on 8.3: utf8mb3_bin is PAD SPACE. A boolean flag could not produce that.

Applied to the VALUES rather than to SqlType::collation, deliberately, so CREATE TABLE ... AS SELECT COLUMN_TYPE FROM information_schema.COLUMNS still reports utf8mb3_bin as 8.3 does.
