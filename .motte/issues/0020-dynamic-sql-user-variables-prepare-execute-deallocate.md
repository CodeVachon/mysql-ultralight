---
id: 20
title: "Dynamic SQL: user variables, PREPARE, EXECUTE, DEALLOCATE"
state: Done
parent: 4
labels: [compat, parser, blocker]
created: 2026-08-18T15:51:57Z
updated: 2026-08-18T18:52:11Z
---

## Description

Discovered by the conformance extraction. 162 of the 173 statements sqlparser-rs rejects are one of two constructs, and both belong to a feature area that was not on the roadmap at all: server-side dynamic SQL.

User-defined variables, 93 statements. Assignment uses the := operator, which sqlparser-rs does not accept in SET:
  SET @fk_exists := (SELECT COUNT(*) FROM INFORMATION_SCHEMA.REFERENTIAL_CONSTRAINTS WHERE ...);

Prepared statements built at runtime, 69 statements each of PREPARE, EXECUTE and DEALLOCATE across 7 migration files:
  SET @sql = IF(@redirect_exists = 0, 'INSERT INTO ...', 'SELECT 1;');
  PREPARE stmt FROM @sql;
  EXECUTE stmt;
  DEALLOCATE PREPARE stmt;

This is how the migrations express conditional DDL: read information_schema into a variable, branch with IF(), build a statement as a string, then prepare and execute it. It requires session-scoped user variables that survive across statements, IF() as a scalar expression, and the ability to parse and execute SQL held in a string at runtime.

Note the interaction with the state image in Phase 6: if migrations are replayed to bake the image, this must work. If the image is produced some other way it can be deferred, but prisma migrate deploy against this engine cannot succeed without it.

## Notes

### 2026-08-18T18:52:11Z — Christopher Vachon (user)

Implemented 2026-08-18. SET @x := expr is rewritten to the = form; PREPARE, EXECUTE with an optional USING list, and DEALLOCATE are recognised before the parser sees them and run against a per-session statement store, with a depth limit so a statement that prepares and executes itself cannot exhaust the stack.

Making the idiom actually work needed two further things the survey had not called out. Scalar subqueries had to be implemented, since the migrations read a count from information_schema into a variable, and correlated subqueries followed from that. SET was also swallowing evaluation errors into NULL, so a failed subquery left the variable NULL and every later branch on it took the wrong path silently.
