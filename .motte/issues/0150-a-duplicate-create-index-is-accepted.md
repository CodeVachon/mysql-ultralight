---
id: 150
title: A duplicate CREATE INDEX is accepted
state: Done
parent: 9
labels: [compat]
created: 2026-09-01T06:09:09Z
updated: 2026-09-03T11:38:50Z
---

## Description

Found while implementing #0137/#0138, unrelated to them.

Creating an index whose name already exists on the table is accepted here; MySQL refuses it with 1061 Duplicate key name.

The same shape as the duplicate-column check #0125 had to hoist out of the CTAS path: a name the engine will answer only one of, accepted twice. Worth checking ALTER TABLE ADD INDEX and ADD CONSTRAINT in the same pass, and whether a duplicate index name across two different tables is legal (it is -- an index name is scoped to its table, unlike a constraint name).

## Notes

### 2026-09-03T11:38:50Z — Christopher Vachon (user)

Landed in 2b443d3. Premise confirmed against 8.3. KEY/UNIQUE/INDEX share one name space and a foreign key participates via the index it implies -- 1061 when it must invent one whose name is taken, accepted when an existing index already covers the columns. Two neighbours the refusal must not swallow: a second PRIMARY KEY is 1068, not 1061, and an unnamed index is named after its first column with _N dedup. Filed #0170-#0177; #0176 (RenameIndex needs the same guard) is the one remaining way to get two indexes of one name into a table, left because exec.rs was being restructured concurrently.

NOTE ON NUMBERING: this agent filed its issues as #0168-#0175 in its worktree, colliding with #0168/#0169 filed in main at the same time. Its eight were renumbered +2 to #0170-#0177 on merge, ids and the one cross-reference updated. Worth avoiding by having agents report findings rather than file them when several run at once.
