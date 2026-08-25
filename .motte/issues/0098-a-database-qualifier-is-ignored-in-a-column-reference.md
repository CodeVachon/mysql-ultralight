---
id: 98
title: A database qualifier is ignored in a column reference
state: Todo
parent: 9
labels: [executor]
created: 2026-08-25T14:48:39Z
updated: 2026-08-25T14:48:39Z
---

## Description

Found while implementing #0058.

    SELECT nodb.J2.b FROM J2   mysql 1054 Unknown column 'nodb.J2.b'   engine answers rows
    SELECT nodb.J2.* FROM J2   mysql 1051 Unknown table 'nodb.J2'      engine answers every column

crates/engine/src/exec.rs's column_slot and Relation::index_of look only at parts[len-2], so the database part of a three-part name is never compared against anything. RelColumn carries a schema field and nothing reads it.

Affects both describe and execute, so #0058's prepare-time resolver does not close it -- that resolver refuses a qualifier that is not in scope, and here the qualifier that is in scope is being matched while the database beside it is discarded.

A wrong answer rather than an error, and the wrong-database case is the one that matters: a statement written against another schema silently reads this one.
