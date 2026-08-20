---
id: 6
title: Phase 5 — Executor, types and functions
state: Done
labels: [epic, phase-5]
blockedBy: [4, 5]
created: 2026-08-18T14:10:54Z
updated: 2026-08-20T14:01:51Z
---

## Description

Query execution plus the MySQL type system and function library, scoped to what the target codebase actually uses rather than all of MySQL 8.

Types observed in the 605 migrations: VARCHAR, DATETIME, BOOLEAN, INTEGER, TEXT/LONGTEXT, ENUM, DOUBLE, JSON, DECIMAL, TINYINT, SMALLINT, DATE, plus character set and collation handling (COLLATE appears 298 times).

Functions observed across migrations and 419 raw-SQL call sites: COALESCE, IFNULL, CONCAT, CAST, ROUND, aggregate functions, GROUP_CONCAT, DATE/DATE_FORMAT/DATE_ADD, REGEXP, LIKE, JSON functions, and the spatial pair ST_Distance_Sphere and POINT. Spatial is confirmed in scope.

## Notes

### 2026-08-20T14:01:51Z — Christopher Vachon (user)

All eleven children are done. The scope this epic named is covered: every type observed in the migrations (VARCHAR, DATETIME, BOOLEAN, INTEGER, TEXT/LONGTEXT, ENUM, DOUBLE, JSON, DECIMAL, TINYINT, SMALLINT, DATE) plus COLLATE, and every function it listed (COALESCE, IFNULL, CONCAT, CAST, ROUND, the aggregates, GROUP_CONCAT, the DATE family, REGEXP, LIKE, the JSON functions, and the spatial pair).

604 migrations apply, prisma db pull reads the real schema as 209 models and 66 enums, conformance is 99.9 percent. The three remaining parse failures are template artifacts, not engine gaps: the statements contain ${...} interpolations, so the text the scanner reconstructs was never valid SQL and the engine never sees it. That figure is the ceiling for static analysis.
