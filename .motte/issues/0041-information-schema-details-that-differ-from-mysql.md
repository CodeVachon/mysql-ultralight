---
id: 41
title: information_schema details that differ from MySQL
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T14:21:40Z
updated: 2026-08-20T14:21:40Z
---

## Description

Found by the differential harness. Three separate small things, grouped because they are all one table's output.

COLUMN_TYPE drops the unsigned marker: a BIGINT UNSIGNED column reads back as 'bigint' where MySQL says 'bigint unsigned'.

A DATE column reports DATETIME_PRECISION 0 where MySQL reports NULL. Note that an existing test in crates/engine/tests/sql.rs asserts 0 with a comment claiming MySQL reports it - that comment is wrong and was written from memory.

The information_schema tables describe their own columns differently: MySQL types COLUMN_TYPE and DATA_TYPE as BLOB and marks several NOT NULL, where this engine reports VAR_STRING and nullable. Whether that matters depends on whether any describing tool reads the metadata of the metadata; Prisma does not.
