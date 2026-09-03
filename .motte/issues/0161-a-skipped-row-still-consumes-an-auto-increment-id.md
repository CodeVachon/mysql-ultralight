---
id: 161
title: A skipped row still consumes an auto-increment id
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-03T03:40:02Z
updated: 2026-09-03T14:20:31Z
---

## Description

Found while implementing #0132, and it changes stored data rather than only a counter.

    INSERT IGNORE of three rows where the first is skipped
    mysql   stores ids 1, 2      engine stores ids 3, 4

MySQL reserves a batch for the statement and hands values only to the rows it actually writes; this engine takes a counter value per row considered, including rows IGNORE or an upsert then discards.

The existing comment at crates/engine/src/exec.rs's assign_auto_increment says 'MySQL still consumes the counter value' -- a correct observation about the counter that led to the wrong mechanism, because what MySQL consumes is not what it stores.

#0132's test an_ignored_row_does_not_report_an_id asserts the relationship rather than the literal ids, so it does not encode the gap.

Note this is distinct from the InnoDB pre-allocation #0132 deliberately did not reproduce (after a two-row INSERT ... SELECT the next id is 4 on 8.3 and 3 here). That one is the allocator and a test pinning it would pin innodb_autoinc_lock_mode; this one is which id a stored row gets.

## Notes

### 2026-09-03T14:01:19Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. INSERT IGNORE (7),(8),(9) with 7 already present: ids 1,2,3 on 8.3 and 1,3,4 here. NOTE for whoever takes this -- the conflicting row must still be present; a probe that deletes it first skips nothing and falsely agrees.
