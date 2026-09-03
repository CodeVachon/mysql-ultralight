---
id: 173
title: DROP INDEX never reports a name that matches nothing
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T10:19:56Z
updated: 2026-09-03T14:20:32Z
---

## Description

Found while implementing #0150. The case-sensitivity half was fixed there, because #0150's duplicate check turned a silent no-op into a refusal: `ALTER TABLE t DROP INDEX I, ADD INDEX i (a)` dropped nothing and then collided with what it had failed to remove. `DropIndex`, `DropConstraint`, `DropForeignKey` and `RenameConstraint` in `crates/engine/src/ddl.rs` now fold case, as `DROP INDEX ... ON` already did and as 8.3 does.

What is left: a name that matches nothing is still accepted here and is an error on MySQL 8.3.

    ALTER TABLE t DROP INDEX nope;
    ERROR 1091 (42000): Can't DROP 'nope'; check that column/key exists

    DROP INDEX nope ON t;
    ERROR 1091 (42000): Can't DROP 'nope'; check that column/key exists

Both measured. `retain` cannot report it — it needs a length or a `position` before the removal. The same is true of `DROP CONSTRAINT` (3940 `Constraint 'x' does not exist.`, measured on a primary key's constraint name) and `DROP FOREIGN KEY` (1091 as well), which this engine also accepts silently.

A migration that drops an index twice is the shape this hides — the second run passes here and fails on the real database.
