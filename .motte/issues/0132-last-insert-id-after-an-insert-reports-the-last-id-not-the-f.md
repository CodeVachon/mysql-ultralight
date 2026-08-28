---
id: 132
title: LAST_INSERT_ID after an INSERT reports the last id, not the first
state: Todo
parent: 9
labels: [compat]
created: 2026-08-27T22:46:01Z
updated: 2026-08-27T22:46:01Z
---

## Description

Found while implementing #0125, which made CTAS follow MySQL's rule and so left the two paths disagreeing with each other.

MySQL reports the FIRST id a statement generated, not the last:

    INSERT ... two auto-increment rows, then LAST_INSERT_ID()
    mysql 1   engine 2

219ecad fixed this for CREATE TABLE ... AS SELECT and deliberately did not touch INSERT, so the engine is now right on one path and wrong on the other. That is worth closing quickly for consistency alone.

Measured alongside it, and separate: after a two-row INSERT ... SELECT the next generated id is 4 on MySQL and 3 here, because InnoDB pre-allocates for a statement whose row count it does not know in advance. A multi-row VALUES insert agrees at 3. CTAS inherits the gap because it now uses the INSERT path, which is the correct outcome rather than a new bug. Worth deciding whether to reproduce that pre-allocation at all -- it is an InnoDB implementation detail, and a test that depends on it is testing the storage engine rather than the SQL.
