---
id: 29
title: "Function library: dates, REGEXP, JSON and spatial"
state: Todo
parent: 6
labels: [executor, compat]
created: 2026-08-18T20:59:12Z
updated: 2026-08-19T12:30:57Z
---

## Description

The function library is the last of Phase 5 that the target codebase actually needs. Implemented today: string, numeric, comparison and session functions, CAST, and GROUP_CONCAT.

Missing, all observed in the 605 migrations and 419 raw-SQL call sites:

- Date and time arithmetic: DATE_FORMAT, DATE_ADD, DATE_SUB, DATEDIFF, TIMESTAMPDIFF, and the HOUR/MINUTE/SECOND/WEEK/QUARTER extractors.
- REGEXP and RLIKE, which sqlparser gives as a binary operator rather than a function.
- JSON: JSON_EXTRACT and the -> and ->> operators, JSON_UNQUOTE, JSON_CONTAINS, JSON_ARRAYAGG, JSON_OBJECT.
- Spatial: ST_Distance_Sphere and POINT, confirmed in scope.

Each unimplemented name must keep erroring rather than returning NULL, so a missing function never reads as an empty result.

## Notes

### 2026-08-18T22:40:24Z — Christopher Vachon (user)

Found by running the real migrations: MD5 and the REGEXP family are now implemented (md-5 and regex crates), as are UUID and RAND, both deterministic. Still missing and now blocking at migration 453 of 604: JSON_TABLE with a JSON path, which is the first hard requirement for the JSON work. The date functions have not come up yet in the migrations, but the application's raw SQL uses them.

### 2026-08-19T12:30:57Z — Christopher Vachon (user)

Done for everything the corpus needs. Implemented: the JSON path subset ($, $.key, $[0], $[*], quoted members), JSON_EXTRACT/UNQUOTE/VALID/QUOTE/ARRAY/OBJECT/LENGTH/TYPE/KEYS/CONTAINS/SET/ARRAY_APPEND/MERGE_PATCH, JSON_ARRAYAGG, the -> and ->> operators, and JSON_TABLE with FOR ORDINALITY. REGEXP and MD5 landed earlier.

Still open and now the only members of this issue: the date-arithmetic functions (DATE_FORMAT, DATE_ADD, DATE_SUB, DATEDIFF, TIMESTAMPDIFF and the extractors) and the spatial pair ST_Distance_Sphere and POINT. Neither appears in the 604 migrations - they are used by the application's raw SQL, so they matter for query-time compatibility rather than for schema setup.
