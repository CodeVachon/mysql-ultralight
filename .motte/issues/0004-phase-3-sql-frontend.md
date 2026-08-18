---
id: 4
title: Phase 3 — SQL frontend
state: Todo
labels: [epic, phase-3]
blockedBy: [2]
created: 2026-08-18T14:10:54Z
updated: 2026-08-18T14:11:32Z
---

## Description

Parser, analyzer, and planner for the MySQL dialect. Reuse sqlparser-rs rather than hand-writing a grammar, extending it where the MySQL dialect falls short.

Surface confirmed as required by the target codebase: DDL including generated columns and full foreign key syntax, correlated subqueries, multi-table UPDATE with JOIN, CTEs including one WITH RECURSIVE, window functions with OVER/PARTITION BY/ROW_NUMBER, EXISTS, UNION, GROUP BY/HAVING, and ON DUPLICATE KEY UPDATE.
