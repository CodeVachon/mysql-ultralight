---
id: 38
title: Every integer column is described as BIGINT on the wire
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T14:21:10Z
updated: 2026-08-20T16:36:46Z
---

## Description

Found by the differential harness. SqlType::column_type maps TypeKind::Int to ColumnType::LongLong at every width, so an INT column is described as MYSQL_TYPE_LONG by MySQL and MYSQL_TYPE_LONGLONG here. TINYINT, SMALLINT and MEDIUMINT are the same.

Sixty-eight of the harness's 103 divergences are column types, and this is the largest share: it fires on nearly every query that selects an integer column.

The declared type is already carried correctly - that was #0035. This is the wire mapping, which needs to branch on base_name the way numeric_precision already does.

## Notes

### 2026-08-20T16:36:46Z — Christopher Vachon (user)

Done. Probing every declared type found 17 of 29 wire codes differed, not just the integers: CHAR against VARCHAR, every TEXT (a BLOB in MySQL), BINARY and VARBINARY, TIMESTAMP against DATETIME, ENUM and SET. column_type now branches on the declared type and 28 of 29 match; FLOAT keeps DOUBLE deliberately (#0048).

It was not safe alone. The binary protocol encodes by the declared code, and the engine happily stored 999 in a TINYINT - so announcing MYSQL_TYPE_TINY would have written one byte and the client would read 231. The range check is #0047, done here.

Declaring TEXT as BLOB then broke Prisma with a base64 blob: MySQL separates a text BLOB from a binary one by charset alone, and ColumnDef::new was setting it from the type code, which cannot tell them apart. OutColumn::to_wire sets it from the declared type now, and temporal types and JSON report the binary collation as MySQL does.

Review found five more places where narrow codes became too narrow, and one process failure that matters more than any of them.

- SUM inherited its argument's width, so SUM over a TINYINT reaching 300 would be sent as one byte and read as 44. MySQL answers NEWDECIMAL for every exact type for exactly this reason. AVG the same.
- The UNSIGNED flag was set by nothing in the workspace. Harmless while every integer was LONGLONG; now an INT UNSIGNED of three billion is written as u32 and read back negative.
- The union widening dropped the declared type, so TEXT UNION LONGTEXT lost the only thing saying the blob carries text - reopening the Prisma failure through another path.
- Widening ignored signedness, and INT UNION INT UNSIGNED has the same code on both sides while neither range holds the other's. Widen also returned early on equal codes, which is what hid it.
- Widening fell back to a string for INT UNION DECIMAL, DOUBLE UNION INT and DATE UNION DATETIME, all of which MySQL aggregates.

The process failure: the differential harness spoke only the text protocol, where a declared type affects nothing about the bytes. Every truncation risk this change introduced lives in the binary protocol, so the harness structurally could not see any of them - and reported 27 divergences fixed and none new while five corruption paths were open. It now runs every case both ways, which immediately surfaced the signedness bug and 143 binary-side entries that were never being compared.

Verified by round-tripping every integer width through the binary protocol at both extremes against MySQL, byte-identical. 604 migrations still apply with range checking on, db pull byte-identical.
