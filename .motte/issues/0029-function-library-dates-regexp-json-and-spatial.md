---
id: 29
title: "Function library: dates, REGEXP, JSON and spatial"
state: Todo
parent: 6
labels: [executor, compat]
created: 2026-08-18T20:59:12Z
updated: 2026-08-18T20:59:12Z
---

## Description

The function library is the last of Phase 5 that the target codebase actually needs. Implemented today: string, numeric, comparison and session functions, CAST, and GROUP_CONCAT.

Missing, all observed in the 605 migrations and 419 raw-SQL call sites:

- Date and time arithmetic: DATE_FORMAT, DATE_ADD, DATE_SUB, DATEDIFF, TIMESTAMPDIFF, and the HOUR/MINUTE/SECOND/WEEK/QUARTER extractors.
- REGEXP and RLIKE, which sqlparser gives as a binary operator rather than a function.
- JSON: JSON_EXTRACT and the -> and ->> operators, JSON_UNQUOTE, JSON_CONTAINS, JSON_ARRAYAGG, JSON_OBJECT.
- Spatial: ST_Distance_Sphere and POINT, confirmed in scope.

Each unimplemented name must keep erroring rather than returning NULL, so a missing function never reads as an empty result.
