---
id: 244
title: 1146 and DML are not refused at prepare
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T08:53:05Z
updated: 2026-09-04T22:02:29Z
---

## Description

Found while implementing #0227, which closed the subquery half of prepare-time resolution. Two categories it deliberately did not cover.

A missing table is not refused at prepare at all:
    SELECT * FROM NoSuch     mysql 1146 at prepare   engine prepares with zero columns
Same for a missing table inside a subquery.

And DML is not DESCRIBED at all, so nothing in it is refused at prepare:
    UPDATE Full SET k = 1 WHERE zz = 1     mysql 1054 at prepare   engine prepares
    DELETE ... WHERE zz = 1, UPDATE ... SET k = zz, INSERT ... VALUES (zz)   same
They ARE refused at execute (#0100's work), so this is about the round trip rather than the answer. Engine::describe returns Ok(None) for anything that is not a Statement::Query.

Priority 2: Prisma prepares every statement, and a migration or query naming a missing table gets no error until it runs.

## Notes

### 2026-09-04T22:02:29Z — Christopher Vachon (user)

Landed, gate green, differential 5398/0. Measured through COM_STMT_PREPARE with a purpose-built probe (crates/differential/examples/prepprobe.rs, kept rather than deleted -- three open issues now turn on prepare-time behaviour).

Both halves fixed: 1146 added to refused_at_prepare (whether a table exists is not something a row or a bound argument can change), and a new read-only resolver for single-table writes. The executor's own paths could NOT be reused -- they take the write lock and work on a copy, so probing one would apply the statement.

THE CORPUS CAUGHT A REGRESSION THIS CHANGE INTRODUCED, and no test of mine would have: 'DELETE FROM a USING S a JOIN T b ON ...' names its target by ALIAS, and the relation lives in the USING clause rather than in from. Looking 'a' up as a table answered 1146 where 8.3 says 1054. Multi-table forms are now skipped rather than guessed at -- that is the smaller half of this issue and remains open.

Remaining: the 1146 message is unqualified here where 8.3 says 'pp.NoSuch' (#0252), and the engine is inconsistent with itself since #0232's 1146 does qualify.
