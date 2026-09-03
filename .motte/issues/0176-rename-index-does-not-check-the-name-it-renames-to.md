---
id: 176
title: RENAME INDEX does not check the name it renames to
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T10:37:23Z
updated: 2026-09-03T14:20:32Z
---

## Description

Found by review while implementing #0150, and deliberately left out of it.

`CREATE TABLE R (a INT, b INT, INDEX i (a), INDEX j (b))` then `ALTER TABLE R RENAME INDEX j TO i` leaves two indexes called `i` here. MySQL 8.3 refuses it with 1061 (42000) `Duplicate key name 'i'`, and `RENAME INDEX j TO I` the same way — measured, case-insensitive like every other spelling of this refusal.

It matters more than a stray name: `DROP INDEX i` removes every index of that name, so a rename that collides can later take an index nobody asked to drop. It is the one way left to put two indexes of one name in a table now that #0150 refuses the rest.

The guard is the same `index_name_taken` #0150 added to `crates/engine/src/ddl.rs` — but the statement is handled in `crates/engine/src/exec.rs` (`Intercepted::RenameIndex`, near line 633), because sqlparser has no `ALTER TABLE ... RENAME INDEX` and `crates/engine/src/rewrite.rs` intercepts it. #0150 was under an instruction not to touch `exec.rs`, which had a change merging concurrently, so the four lines were left for whoever picks this up.

`AlterTableOperation::RenameConstraint` in `ddl.rs` renames an index too and has the same hole, but MySQL has no `RENAME CONSTRAINT` at all, so there is nothing to match there.
