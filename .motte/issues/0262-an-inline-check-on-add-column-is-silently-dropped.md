---
id: 262
title: An inline CHECK on ADD COLUMN is silently dropped
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-08T17:51:44Z
updated: 2026-09-08T17:51:44Z
---

## Description

Found by review on 2026-09-08 while working #0257, and pre-existing.

    ALTER TABLE t ADD COLUMN b INT CHECK (b > 0)

parses, reports success, and creates the column without the check. `build_column` -- the wrapper `ADD COLUMN` and `MODIFY COLUMN` call -- makes a throwaway `inline_checks` vector and drops it on the floor:

    fn build_column(def: &ColumnDef, position: usize) -> EngineResult<Column> {
        let mut inline_checks = Vec::new();
        build_column_with_checks(def, position, &mut inline_checks)
    }

`CREATE TABLE` goes through `build_column_with_checks` directly and keeps them, which is why the same clause works there.

This is a wrong answer rather than a clear error, which the project's conventions rule out: a client is told the constraint exists and rows violating it are then accepted.

The fix is to thread the collected checks out to the ALTER path and name any unnamed one with `auto_check_name` against the table being altered, which is what `create_table` now does since #0257. Whether MySQL numbers a check added by ALTER from the table's existing highest suffix has not been measured -- probe it before implementing.

Priority 3: Prisma writes its constraints table-level, and the 604-migration history applies. It matters to anything hand-written.
