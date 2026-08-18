---
id: 4
title: Phase 3 — SQL frontend
state: Done
labels: [epic, phase-3]
blockedBy: [2]
created: 2026-08-18T14:10:54Z
updated: 2026-08-18T20:17:13Z
---

## Description

Parser, analyzer, and planner for the MySQL dialect. Reuse sqlparser-rs rather than hand-writing a grammar, extending it where the MySQL dialect falls short.

Surface confirmed as required by the target codebase: DDL including generated columns and full foreign key syntax, correlated subqueries, multi-table UPDATE with JOIN, CTEs including one WITH RECURSIVE, window functions with OVER/PARTITION BY/ROW_NUMBER, EXISTS, UNION, GROUP BY/HAVING, and ON DUPLICATE KEY UPDATE.

## Notes

### 2026-08-18T20:17:13Z — Christopher Vachon (user)

Complete 2026-08-18 for the scope that matters: the engine's front end parses every one of the 3,161 statements in the 604 migrations and every one of the 1,191 in the provisioning dumps. sql_mode is threaded into parsing, the five gaps the conformance survey found are closed, and the three statements still rejected across the corpus are Prisma.sql template artifacts where the complete statement never exists in source.
