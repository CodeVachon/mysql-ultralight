---
id: 259
title: A CHECK constraint is invisible to information_schema
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-08T17:11:54Z
updated: 2026-09-08T17:11:54Z
---

## Description

Found on 2026-09-08 while adding coverage for #0257, with scripts/ask.sh. Pre-existing and separate from that ticket: the engine records CHECK constraints and enforces them, and refuses a duplicate name since #0257, but nothing can read one back.

    CREATE TABLE T (id INT NOT NULL PRIMARY KEY, r INT NULL);
    ALTER TABLE T ADD CONSTRAINT ck1 CHECK (r > 0);

    SELECT CONSTRAINT_NAME, CONSTRAINT_TYPE FROM information_schema.TABLE_CONSTRAINTS
      WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'T';
      mysql   ck1 CHECK, PRIMARY PRIMARY KEY
      engine  PRIMARY PRIMARY KEY only

    SELECT CONSTRAINT_NAME, CHECK_CLAUSE FROM information_schema.CHECK_CONSTRAINTS
      WHERE CONSTRAINT_SCHEMA = DATABASE();
      mysql   ck1  (`r` > 0)
      engine  nothing at all

So two gaps: TABLE_CONSTRAINTS does not report the CHECK row, and CHECK_CONSTRAINTS is not a table this engine has.

CHECK_CLAUSE is the harder half. 8.3 reports the expression as *it* renders it -- `(`r` > 0)`, with the identifier quoted and the whole thing parenthesised -- and `catalog::Check` holds the expression as a String. Whether that string round-trips to MySQL's rendering has not been measured, and this issue should not be closed on a CHECK_CLAUSE that merely looks close.

Priority 3: Prisma reads TABLE_CONSTRAINTS for keys and indexes, and the 604-migration history applies with checks invisible. This is a gap for anything that introspects checks, not for the target application.
