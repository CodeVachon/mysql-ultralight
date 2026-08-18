---
id: 16
title: information_schema as first-class queryable tables
state: Done
parent: 4
labels: [compat, prisma, blocker]
created: 2026-08-18T14:41:18Z
updated: 2026-08-18T18:13:30Z
---

## Description

Discovered by Track C: prisma migrate and prisma db push are information_schema clients. Before emitting any DDL the migration engine runs nine introspection queries against VIEWS, TABLES, COLUMNS, TABLE_CONSTRAINTS, CHECK_CONSTRAINTS, KEY_COLUMN_USAGE, REFERENTIAL_CONSTRAINTS, STATISTICS and ROUTINES.

This is a prerequisite for the first Phase 8 milestone, not a late-stage nicety, and it cannot be special-cased: the queries join information_schema tables to each other and use BINARY comparison in select lists, join conditions, WHERE and ORDER BY, plus LOWER(), IF() and DISTINCT. They must run through the real executor over catalog data exposed as ordinary tables.

Exact statements captured in docs/spike-wire-protocol.md.

## Notes

### 2026-08-18T18:13:30Z — Christopher Vachon (user)

Implemented 2026-08-18 in crates/engine/src/info_schema.rs, generated from the catalog on read so ordinary query execution applies. SCHEMATA, TABLES, COLUMNS, STATISTICS, TABLE_CONSTRAINTS, KEY_COLUMN_USAGE and REFERENTIAL_CONSTRAINTS carry real data; VIEWS, ROUTINES, CHECK_CONSTRAINTS, TRIGGERS and EVENTS are present but empty, which is what Prisma needs to conclude none are defined. Joining these tables to each other is what forced JOIN support into the prototype, and prisma db push now completes against the engine.
