---
id: 50
title: information_schema describes its own columns differently
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T18:45:13Z
updated: 2026-08-21T14:59:11Z
---

## Description

Split out of #0041, which fixed the *values* that table reports. This is about how those columns are themselves described on the wire.

MySQL types COLUMN_NAME, DATA_TYPE and COLUMN_TYPE as BLOB where this engine sends VAR_STRING, marks several NOT NULL where this engine says nullable, and marks CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION and NUMERIC_SCALE UNSIGNED where this engine sends them signed. NUMERIC_SCALE is a LONG in MySQL and a LONGLONG here.

Why it was split rather than done with the rest: info_schema::build infers each column's type from the data in the rows it was handed, so there is no place to declare any of this. Getting it right means giving every information_schema table a real column spec, and measuring each column of each table against MySQL to fill it in — the divergences above are only the ones the corpus happens to select.

Nothing is known to read it. Prisma does not, and the engine's own describe path does not. The baseline carries 24 lines for it.

## Notes

### 2026-08-21T14:59:11Z — Christopher Vachon (user)

All 22 lines fixed; baseline 37 to 15. Each information_schema column is declared as MySQL declares it, read from MySQL's own catalogue rather than inferred from whatever rows a query returned. Worth knowing for anyone revisiting: MySQL does not answer this consistently — a plain scan narrows a column to the smallest type its rows need, and materialising the result, which ORDER BY ORDINAL_POSITION forces and every introspection query does, falls back to the declared one. NUMERIC_PRECISION is a LONG under a plain scan and a LONGLONG under that ORDER BY. The declared type is recorded, and the two text columns that narrow even when materialised are spelled as they arrive.
