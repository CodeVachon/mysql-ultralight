---
id: 153
title: Multi-table DELETE drops the database qualifier
state: In Progress
parent: 9
labels: [executor]
created: 2026-09-02T14:59:43Z
updated: 2026-09-03T05:25:45Z
---

## Description

Found by review during #0108, against 75b620a (#0098).

    DELETE nodb.A FROM A JOIN B ON A.id = B.id
    mysql 1109   engine deletes from the current schema

#0098 taught column references to compare the database part of a three-part name, and covered the INSERT column list, both UPDATE targets and the ODKU target. The multi-table DELETE target was missed: crates/engine/src/exec.rs maps delete.tables through name::tail, and target_name does the same, so the database is discarded exactly as column_slot used to do.

A wrong answer on a write, and the worst spelling of it -- a DELETE aimed at another schema silently deletes from this one.

The fix is the one #0098 already built: compare through the qualifier rather than taking the tail. Note the error code differs from the column case -- MySQL answers 1109 Unknown table here, not 1054.
