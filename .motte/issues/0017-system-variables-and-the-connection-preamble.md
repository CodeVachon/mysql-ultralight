---
id: 17
title: System variables and the connection preamble
state: Done
parent: 6
labels: [compat, prisma]
created: 2026-08-18T14:41:18Z
updated: 2026-08-18T18:13:30Z
---

## Description

Discovered by Track C: every Prisma connection opens with SELECT @@socket, @@max_allowed_packet, @@wait_timeout. If that returns zero rows, mysql_async inside Prisma's query engine calls Option::unwrap() on the missing row and panics rather than returning an error. System variables are therefore the first hard requirement on the wire, before any SQL feature.

Also required: @@version, @@GLOBAL.version, @@lower_case_table_names, @@sql_mode, and the character set and collation variables. Scope resolution must accept @@x, @@GLOBAL.x and @@SESSION.x, and the select list must tolerate aliases by juxtaposition (SELECT @@GLOBAL.version version) as well as the AS form, because Prisma's migration engine emits both.

The spike implements a working subset in crates/cli/src/spike.rs; the real engine needs this backed by a proper session variable store with correct column types.

## Notes

### 2026-08-18T18:13:30Z — Christopher Vachon (user)

Implemented 2026-08-18 in crates/engine/src/session.rs. System variables resolve through a single table with GLOBAL/SESSION/LOCAL scope prefixes normalised away, including the SELECT @@socket, @@max_allowed_packet, @@wait_timeout preamble that panics Prisma's driver when it returns no row. Unknown names error with 1193 rather than returning a plausible value. User variables persist across statements. SHOW VARIABLES enumerates the set.
