---
id: 153
title: Multi-table DELETE drops the database qualifier
state: Done
parent: 9
labels: [executor]
created: 2026-09-02T14:59:43Z
updated: 2026-09-03T08:25:45Z
---

## Description

Found by review during #0108, against 75b620a (#0098).

    DELETE nodb.A FROM A JOIN B ON A.id = B.id
    mysql 1109   engine deletes from the current schema

#0098 taught column references to compare the database part of a three-part name, and covered the INSERT column list, both UPDATE targets and the ODKU target. The multi-table DELETE target was missed: crates/engine/src/exec.rs maps delete.tables through name::tail, and target_name does the same, so the database is discarded exactly as column_slot used to do.

A wrong answer on a write, and the worst spelling of it -- a DELETE aimed at another schema silently deletes from this one.

The fix is the one #0098 already built: compare through the qualifier rather than taking the tail. Note the error code differs from the column case -- MySQL answers 1109 Unknown table here, not 1054.

## Notes

### 2026-09-03T08:25:44Z — Christopher Vachon (user)

Done in 5fb40fd, in an isolated worktree, after three review passes.

#0098 built the comparison; this is the DELETE target it missed. delete.tables and target_name both mapped through name::tail, discarding the database exactly as column_slot used to. MySQL answers 1109 Unknown table here rather than the 1054 a column gets.

The third review pass found the bug surviving in the one spelling that never reached either new check: an aliased USING target built DeleteTarget from the alias and never looked at the written name, so DELETE FROM nodb.J2 j USING J2 j JOIN K deleted a row from the session's schema.

That was settled by measurement rather than patched around: a USING target may not carry an alias at all. Every spelling is 1064 on 8.3 -- with the database wrong, with it right, with AS, with three parts, and in a two-target list -- while the unaliased form deletes. So target_written refuses an alias and falls through to the written name: the targets come before USING, and the relation with its aliases comes after. Strictly closer to 8.3 than accepting the alias and reading it as the whole target.

Also fixed: both guards indexed source_databases directly, reintroducing the parallel-vector assumption the target walk avoids by zipping. They go through Relation::source_holds now, so a future FROM item that pushes a source without a database becomes a refusal rather than a panic partway through a delete.

17 mutations, all bite; one deliberately non-behavioural mutation correctly did not and is not counted.

One deliberate divergence filed as #0166: a multi-table write whose targets span databases is refused with 1235, because this engine commits one database per statement. MySQL executes it. Before this change the database was discarded and the statement wrote to the current schema, so the refusal is better than what it replaced and worse than MySQL.

Honest leftovers: 1146-vs-1109 ordering when a FROM table is missing, a view as a DELETE target, and DELETE FROM x.* USING.

Corpus 3208 -> 3284, baseline empty, both gates verified independently on the merged tree.
