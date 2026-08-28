---
id: 133
title: ALTER TABLE DROP COLUMN of a missing column is 1054, not 1091
state: Todo
parent: 9
labels: [compat]
created: 2026-08-27T22:46:01Z
updated: 2026-08-27T22:46:01Z
---

## Description

Found while implementing #0125.

    ALTER TABLE t DROP COLUMN nosuch
    mysql   1091 Can't DROP 'nosuch'; check that column/key exists
    engine  1054 Unknown column 'nosuch' in 'field list'

Both refuse, so this is a wrong code and message rather than a wrong answer. 1054 is the field-list error a SELECT gives; MySQL has a distinct code for a DROP naming something absent, and it covers keys as well as columns.

Recorded in the test comment at the site so the assertion is not quietly claiming MySQL's answer. Worth checking DROP INDEX and DROP FOREIGN KEY of a missing name in the same pass -- 1091 is the shared code for all three in MySQL.
