---
id: 73
title: An all-NULL union is typed two ways by MySQL
state: Todo
parent: 9
labels: [compat]
created: 2026-08-21T18:26:27Z
updated: 2026-08-21T18:26:27Z
---

## Description

SELECT NULL UNION SELECT NULL
    mysql, prepared   MYSQL_TYPE_NULL
    mysql, executed   MYSQL_TYPE_STRING
    engine, both      MYSQL_TYPE_NULL

Found while adding corpus coverage for #0065. A union materialises, and a materialised column cannot carry the null type, so MySQL's execution path reports a string where its own prepare path reports NULL. `SELECT NULL` on its own is NULL in both.

The engine has one answer for both paths and currently gives NULL, which matches prepare and not execute. Matching execute instead would make `describe` disagree with MySQL's prepare, so neither choice is right — the same shape as the information_schema types in #0050, where a plain scan and a materialised one disagree.

Left out of the differential corpus on purpose: a statement MySQL answers two ways cannot distinguish an engine bug from a MySQL inconsistency. crates/engine/tests/sql.rs pins the engine's behaviour so a change to it is at least deliberate.
