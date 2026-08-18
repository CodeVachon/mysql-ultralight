---
id: 20
title: "Dynamic SQL: user variables, PREPARE, EXECUTE, DEALLOCATE"
state: Todo
parent: 4
labels: [compat, parser, blocker]
created: 2026-08-18T15:51:57Z
updated: 2026-08-18T15:51:57Z
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
