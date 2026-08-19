---
id: 31
title: Run the real 604-migration history end to end
state: Done
parent: 8
labels: [compat, prisma, milestone]
created: 2026-08-18T22:40:24Z
updated: 2026-08-19T12:48:15Z
---

## Description

Pointing `prisma migrate deploy` at the engine with the real Appello history is the only test that proves the engine works, as opposed to parses. 453 of 604 migrations apply today.

Everything it found so far is fixed: multi-statement commands, CTEs, INSERT ... SELECT, named locks, UUID/RAND/MD5/REGEXP, DROP INDEX, the implicit index MySQL creates for a foreign key, inline UNIQUE INDEX names, table aliases in single-table UPDATE and DELETE, ON UPDATE CURRENT_TIMESTAMP, VALUES ROW(), CAST AS SIGNED, functional dependency under ONLY_FULL_GROUP_BY, and INSERT IGNORE filling a NOT NULL column that has no default. A deadlock was also found and fixed: every writer held the catalog's write lock while evaluating, so any subquery in VALUES, SET or WHERE hung the connection forever.

Next blocker at migration 453: JSON_TABLE, which needs JSON path support — see #0029.

This should become an automated check once it passes, because it is the difference between a parser and a database.

## Notes

### 2026-08-18T23:01:22Z — Christopher Vachon (user)

Reviewed. Twelve findings, all fixed with regression tests. The worst was introduced by the deadlock fix: writers computed against a copy with no lock held, so two connections discarded each other's rows and both reported success — four threads inserting 200 rows each left 325 of 800. Writers now hold a gate across the whole copy-compute-commit cycle, and crates/engine/tests/concurrency.rs reproduces the loss.

Also fixed: ONLY_FULL_GROUP_BY propagated functional dependency both ways through an outer join, permitting a genuinely ambiguous column; RAND() was effectively constant; CAST('12abc' AS SIGNED) returned NULL instead of 12; a CTE inside a subquery was out of scope; CTEs were materialised twice; COM_SET_OPTION was ignored, so a client that turned multi-statements off still got them; and a multi-statement command that failed part way reported that nothing had run.

### 2026-08-19T12:30:57Z — Christopher Vachon (user)

All 604 migrations now apply, and re-running prisma migrate deploy against a server booted from the resulting image answers 'No pending migrations to apply' - the image is a verified replacement for the migration step, not an approximation of it.

The last 151 needed: JSON_TABLE (implicitly LATERAL, so it is expanded per row of the tables named before it, and per row of the enclosing query when it is alone in a subquery's FROM), the JSON function library, comma-separated FROM items, COLLATE in an expression, and ON DUPLICATE KEY UPDATE.

Final numbers: 13.6s to apply all 604 against an empty engine, 5ms to snapshot, 918 KB image, 1.3-1.7ms to boot from it. 278 tables, 2771 columns, 604 migration records.
