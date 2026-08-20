---
id: 50
title: information_schema describes its own columns differently
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T18:45:13Z
updated: 2026-08-20T18:45:13Z
---

## Description

Split out of #0041, which fixed the *values* that table reports. This is about how those columns are themselves described on the wire.

MySQL types COLUMN_NAME, DATA_TYPE and COLUMN_TYPE as BLOB where this engine sends VAR_STRING, marks several NOT NULL where this engine says nullable, and marks CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION and NUMERIC_SCALE UNSIGNED where this engine sends them signed. NUMERIC_SCALE is a LONG in MySQL and a LONGLONG here.

Why it was split rather than done with the rest: info_schema::build infers each column's type from the data in the rows it was handed, so there is no place to declare any of this. Getting it right means giving every information_schema table a real column spec, and measuring each column of each table against MySQL to fill it in — the divergences above are only the ones the corpus happens to select.

Nothing is known to read it. Prisma does not, and the engine's own describe path does not. The baseline carries 24 lines for it.
