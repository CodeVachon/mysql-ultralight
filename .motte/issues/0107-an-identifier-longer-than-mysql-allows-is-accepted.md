---
id: 107
title: An identifier longer than MySQL allows is accepted
state: In Progress
parent: 9
labels: [compat]
created: 2026-08-25T16:39:41Z
updated: 2026-09-03T08:27:05Z
---

## Description

Two related limits found while measuring name lengths for #0059, which implemented the third.

    SELECT 1 AS <300 chars>              mysql reports the name cut at 256   engine reports all 300
    CREATE TABLE t (`<300 chars>` INT)   mysql ERROR 1059                    engine accepts, 300-char column

#0059 implemented MySQL's 255-byte cut for a *generated* column name -- the name MySQL invents for an unaliased expression -- and deliberately left these two alone, because they are a different limit on a different path: 256 for a reported alias, and a hard 1059 refusal at DDL.

The DDL half is the one that matters: accepting a table this engine can create and MySQL cannot means a migration that passes here fails against a real server, which is the opposite of what this engine is for.
