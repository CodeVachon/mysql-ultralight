---
id: 16
title: information_schema as first-class queryable tables
state: Todo
parent: 4
labels: [compat, prisma, blocker]
created: 2026-08-18T14:41:18Z
updated: 2026-08-18T14:41:18Z
---

## Description

Discovered by Track C: prisma migrate and prisma db push are information_schema clients. Before emitting any DDL the migration engine runs nine introspection queries against VIEWS, TABLES, COLUMNS, TABLE_CONSTRAINTS, CHECK_CONSTRAINTS, KEY_COLUMN_USAGE, REFERENTIAL_CONSTRAINTS, STATISTICS and ROUTINES.

This is a prerequisite for the first Phase 8 milestone, not a late-stage nicety, and it cannot be special-cased: the queries join information_schema tables to each other and use BINARY comparison in select lists, join conditions, WHERE and ORDER BY, plus LOWER(), IF() and DISTINCT. They must run through the real executor over catalog data exposed as ordinary tables.

Exact statements captured in docs/spike-wire-protocol.md.
