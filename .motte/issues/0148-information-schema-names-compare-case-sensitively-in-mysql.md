---
id: 148
title: information_schema names compare case-sensitively in MySQL
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-01T06:09:08Z
updated: 2026-09-03T14:20:30Z
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
