---
id: 98
title: A database qualifier is ignored in a column reference
state: Done
parent: 9
labels: [executor]
created: 2026-08-25T14:48:39Z
updated: 2026-09-01T23:47:05Z
---

## Description

Found while implementing #0058.

    SELECT nodb.J2.b FROM J2   mysql 1054 Unknown column 'nodb.J2.b'   engine answers rows
    SELECT nodb.J2.* FROM J2   mysql 1051 Unknown table 'nodb.J2'      engine answers every column

crates/engine/src/exec.rs's column_slot and Relation::index_of look only at parts[len-2], so the database part of a three-part name is never compared against anything. RelColumn carries a schema field and nothing reads it.

Affects both describe and execute, so #0058's prepare-time resolver does not close it -- that resolver refuses a qualifier that is not in scope, and here the qualifier that is in scope is being matched while the database beside it is discarded.

A wrong answer rather than an error, and the wrong-database case is the one that matters: a statement written against another schema silently reads this one.

## Notes

### 2026-09-01T23:47:05Z — Christopher Vachon (user)

Done in 75b620a.

RelColumn already carried a schema and nothing read it, but it needed a second field rather than that one: they are different facts. 8.3 reports a derived table's column as belonging to the current schema while still answering nodb.d.b, so what a column reports and what it answers to are not the same.

The asymmetry that shaped the fix: a FROM item in no database -- derived table, CTE, JSON_TABLE -- reads the absence two opposite ways. A column qualifier matches anything, because there is nothing to compare against; a wildcard qualifier matches nothing, so probe.d.* is 1051 even with the correct database. Two comparers, not one shared rule.

The brief's premise that MySQL refuses an aliased three-part name was wrong: SELECT probe.x.b FROM J2 x answers on 8.3, because an alias renames the table part and keeps the database. SELECT probe.x.z FROM other.T x is 1054, so the database is still compared.

Every write path had to resolve its target before reading a row -- the ODKU target settled inside the collision branch and the single-table SET target inside the row loop, so the refusal only arrived when there was something to refuse. That is the epic's zero-rows pattern (see the note on #0009), checked deliberately against an empty table, WHERE 1=0, LIMIT 0 and an untaken CASE branch.

Still answering, deliberately: UPDATE E SET b=1 WHERE nodb.E.a=1 over an empty table, because the predicate is read per row -- that is #0102, noted beside the existing guard.

Kept case-insensitive for identifiers, consistent with every other identifier comparison in the engine, where the container's 8.3 runs lower_case_table_names=0 and is case-sensitive. Filed as #0151.

15 mutations, all bite. Corpus 2288 -> 2420, baseline empty, both gates verified independently.
