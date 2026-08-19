---
id: 29
title: "Function library: date arithmetic and spatial"
state: Todo
parent: 6
labels: [executor, compat]
created: 2026-08-18T20:59:12Z
updated: 2026-08-19T19:19:29Z
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

### 2026-08-19T19:19:29Z — Christopher Vachon (user)

Landed. Implemented in crates/engine/src/temporal.rs plus the function layer: DATE_FORMAT with the specifier set the corpus uses, DATE_ADD/DATE_SUB/ADDDATE/SUBDATE/TIMESTAMPADD, DATEDIFF, TIMESTAMPDIFF, and the extractors HOUR/MINUTE/SECOND/MICROSECOND/DAYOFWEEK/WEEKDAY/DAYOFYEAR/QUARTER/LAST_DAY. Spatial: POINT held as WKT text and ST_Distance_Sphere on MySQL's 6,370,986 m sphere with its longitude/latitude bounds and error codes 3616/3617.

WEEK() is not implemented and is not needed: the six apparent hits in the corpus are all prose, a TypeScript .week() call and a startOfWeek setting, with no SQL among them.

Calendar arithmetic is hand-rolled on days_from_civil/civil_from_days rather than a date crate - one format and a handful of operations, against a dependency paid on every cold start. Month arithmetic clamps, so 31 Jan plus a month is 28 Feb.

Two bugs found before review, both wrong answers rather than errors: day zero parsed as the last of the previous month (2026-01-00 read as 2025-12-31), and a day of 4294967296 wrapping through 'as u32' to zero and taking the same path; and a near-antipodal pair driving the haversine term one ulp above 1, where asin is NaN, so the distance came back NaN instead of half the circumference.

Review found seven more. The interesting ones: interval arithmetic multiplied unchecked, so INTERVAL 9223372036854775807 YEAR panicked in debug and wrapped to a garbage date in release; a FROM-less SELECT folded aggregates over zero rows, so SELECT COUNT(*) was 0 where MySQL says 1, and the aggregate's argument was never evaluated at all; and the ONLY_FULL_GROUP_BY walk read TIMESTAMPDIFF's bare unit keyword as a column, making the function unusable in a grouped projection.

The behavioural decision worth recording: a malformed datetime inside an expression is now NULL, not error 1292. Strict mode governs data-change statements, not expression evaluation - MySQL answers SELECT DATE_ADD('2026-13-45', INTERVAL 1 DAY) with NULL and a warning. YEAR/MONTH/DAY were moved onto the same parser so one bad value no longer behaves two ways: they used to split on '-' and report 2026 for '2026-13-45' while QUARTER of the same text was NULL. Unimplemented units still error, because that is a gap in the engine rather than a value MySQL would call NULL.

604 migrations still apply, conformance unchanged at 99.9 percent, 218 engine tests.
