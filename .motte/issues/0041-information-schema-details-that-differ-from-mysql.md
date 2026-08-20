---
id: 41
title: information_schema details that differ from MySQL
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T14:21:40Z
updated: 2026-08-20T19:00:26Z
---

## Description

Found by the differential harness. Three separate small things, grouped because they are all one table's output.

COLUMN_TYPE drops the unsigned marker: a BIGINT UNSIGNED column reads back as 'bigint' where MySQL says 'bigint unsigned'.

A DATE column reports DATETIME_PRECISION 0 where MySQL reports NULL. Note that an existing test in crates/engine/tests/sql.rs asserts 0 with a comment claiming MySQL reports it - that comment is wrong and was written from memory.

The information_schema tables describe their own columns differently: MySQL types COLUMN_TYPE and DATA_TYPE as BLOB and marks several NOT NULL, where this engine reports VAR_STRING and nullable. Whether that matters depends on whether any describing tool reads the metadata of the metadata; Prisma does not.

## Notes

### 2026-08-20T19:00:26Z — Christopher Vachon (user)

Every column type verified against MySQL 8.3 side by side. Found five more than the issue listed: VARBINARY lost its length, SET lost its member quotes, DECIMAL/FLOAT/DOUBLE UNSIGNED were rejected outright, DECIMAL(8) reported decimal(8) not decimal(8,0), FLOAT(7,3) lost both. REAL was a synonym for FLOAT where MySQL makes it DOUBLE. The metadata-of-the-metadata part is split out as #0050.
