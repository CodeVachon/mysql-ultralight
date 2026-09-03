---
id: 172
title: An index may be called PRIMARY
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T10:19:46Z
updated: 2026-09-03T10:37:49Z
---

## Description

Found while implementing #0150. The other half of this — the primary key's index being stored under the constraint's name — was fixed there after review found it could cost the key: with `CONSTRAINT mypk PRIMARY KEY (a), INDEX mypk (b)` in the table, `DROP INDEX mypk` removed both entries and the primary key silently stopped being enforced. The index is now always called `PRIMARY`, which is what 8.3 calls it (`DROP CONSTRAINT mypk` there is 3940 `Constraint 'mypk' does not exist.`).

What is left: on MySQL 8.3 no *other* index may be called `PRIMARY`.

    CREATE TABLE t (a INT NOT NULL, b INT, PRIMARY KEY (a), INDEX `PRIMARY` (b));
    ERROR 1280 (42000): Incorrect index name 'PRIMARY'

``primary`` is refused the same way, so the check folds case. This engine accepts both, and since #0150 leaves primary indexes out of the duplicate-name space to keep a second PRIMARY KEY answering 1068 rather than 1061 (#0170), an index called `PRIMARY` lands beside the key rather than colliding with it — which `DROP INDEX PRIMARY` would then take away together.

1280 is a check on the name itself, before any collision: `CREATE TABLE t (a INT, INDEX `PRIMARY` (a))` with no primary key at all is 1280 too.
