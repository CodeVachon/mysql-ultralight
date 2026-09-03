---
id: 107
title: An identifier longer than MySQL allows is accepted
state: Done
parent: 9
labels: [compat]
created: 2026-08-25T16:39:41Z
updated: 2026-09-03T11:12:06Z
---

## Description

Two related limits found while measuring name lengths for #0059, which implemented the third.

    SELECT 1 AS <300 chars>              mysql reports the name cut at 256   engine reports all 300
    CREATE TABLE t (`<300 chars>` INT)   mysql ERROR 1059                    engine accepts, 300-char column

#0059 implemented MySQL's 255-byte cut for a *generated* column name -- the name MySQL invents for an unaliased expression -- and deliberately left these two alone, because they are a different limit on a different path: 256 for a reported alias, and a hard 1059 refusal at DDL.

The DDL half is the one that matters: accepting a table this engine can create and MySQL cannot means a migration that passes here fails against a real server, which is the opposite of what this engine is for.

## Notes

### 2026-09-03T11:12:06Z — Christopher Vachon (user)

Landed in 9519ca0. Measured on 8.3 rather than assumed: the limit is 64 *characters*, not 256 and not bytes, and it is answered 1059 where a name is defined and 1166 where a column name is derived. A select-item alias is a third case -- cut at 256 bytes, not refused, and cut in the catalog rather than only in the report.

Three code-review findings fixed: a column-level CONSTRAINT name escaped the check entirely (the check read TableConstraint, this is a ColumnOptionDef), and the view alias walk stopped at subqueries written inside expressions. The third finding's premise was wrong -- defining an over-long CTE name is legal on 8.3; it is *referencing* one that is 1059 -- so a comment asserting the opposite was corrected and the real divergence filed as #0169.

Tests proven with mutate.sh: both call sites, characters-vs-bytes, and the 256/255 boundary each fail without their code. Differential 3392 statements, 0 divergences.
