---
id: 133
title: ALTER TABLE DROP COLUMN of a missing column is 1054, not 1091
state: Done
parent: 9
labels: [compat]
created: 2026-08-27T22:46:01Z
updated: 2026-09-03T12:51:10Z
---

## Description

Found while implementing #0125.

    ALTER TABLE t DROP COLUMN nosuch
    mysql   1091 Can't DROP 'nosuch'; check that column/key exists
    engine  1054 Unknown column 'nosuch' in 'field list'

Both refuse, so this is a wrong code and message rather than a wrong answer. 1054 is the field-list error a SELECT gives; MySQL has a distinct code for a DROP naming something absent, and it covers keys as well as columns.

Recorded in the test comment at the site so the assertion is not quietly claiming MySQL's answer. Worth checking DROP INDEX and DROP FOREIGN KEY of a missing name in the same pass -- 1091 is the shared code for all three in MySQL.

## Notes

### 2026-09-03T12:51:09Z — Christopher Vachon (user)

Landed in 451f7df, gate green on main. Premise held but understated it: 1091 is shared by five drops, and there are four distinct codes, not one (1091, 3940, 3939, plus 1054 kept for MODIFY/CHANGE/RENAME COLUMN). Four of the seven cases were not refusals at all -- DROP INDEX/FOREIGN KEY/PRIMARY KEY of a missing name were accepted silently, so a migration naming an index that is not there passed here and fails against a real server.

DROP CONSTRAINT's name space is exactly what information_schema TABLE_CONSTRAINTS lists, and it is settled in a phase before every operation in the loop -- measurable because a doubled DROP CONSTRAINT is accepted while a doubled DROP COLUMN is 1091 on the second.

The agent caught its own mismeasurement: reusing one table across cases made DROP CONSTRAINT `PRIMARY` read as 3940 when the preceding statement had already dropped the key. Re-measured with a fresh table per case.

14 mutations. Divergences found and filed centrally as #0178-#0185.
