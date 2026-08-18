---
id: 6
title: Phase 5 — Executor, types and functions
state: Todo
labels: [epic, phase-5]
blockedBy: [4, 5]
created: 2026-08-18T14:10:54Z
updated: 2026-08-18T14:11:32Z
---

## Description

Query execution plus the MySQL type system and function library, scoped to what the target codebase actually uses rather than all of MySQL 8.

Types observed in the 605 migrations: VARCHAR, DATETIME, BOOLEAN, INTEGER, TEXT/LONGTEXT, ENUM, DOUBLE, JSON, DECIMAL, TINYINT, SMALLINT, DATE, plus character set and collation handling (COLLATE appears 298 times).

Functions observed across migrations and 419 raw-SQL call sites: COALESCE, IFNULL, CONCAT, CAST, ROUND, aggregate functions, GROUP_CONCAT, DATE/DATE_FORMAT/DATE_ADD, REGEXP, LIKE, JSON functions, and the spatial pair ST_Distance_Sphere and POINT. Spatial is confirmed in scope.
