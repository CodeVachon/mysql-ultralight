---
id: 175
title: ALTER TABLE ADD COLUMN with an inline UNIQUE or PRIMARY KEY creates no index
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T10:37:11Z
updated: 2026-09-03T14:20:32Z
---

## Description

Found by review while implementing #0150, and left out of it: #0150 covers the paths that create an index, and this is the one that should and does not.

`ALTER TABLE t ADD COLUMN b INT UNIQUE` sets the column's `unique` flag here and creates no index entry, so `information_schema.columns` reports `COLUMN_KEY = UNI` while duplicate values insert without complaint — uniqueness in this engine is enforced by walking the indexes, and there is nothing to walk. `ADD COLUMN b INT NOT NULL PRIMARY KEY` is the same: no index, and duplicate keys accepted.

Measured on mysql:8.3: the first gives an index called `b` (`Non_unique: 0`), the second one called `PRIMARY`.

`crates/engine/src/ddl.rs` already builds both of these for `CREATE TABLE` — the loop above the constraints for a column-level UNIQUE, and the `inline_pk` block under them — and #0150 gave that file `add_index` and `auto_index_name`, which name and check what they add. `AlterTableOperation::AddColumn` is the one path neither was applied to. The name a new column's unique index should get is the column's, through `auto_index_name`, since the table it joins may already carry that name.
