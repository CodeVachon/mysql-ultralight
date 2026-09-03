---
id: 193
title: ON DUPLICATE KEY UPDATE does not restamp ON UPDATE CURRENT_TIMESTAMP
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T15:12:57Z
updated: 2026-09-03T15:12:57Z
---

## Description

Found by review during #0089 and confirmed by measurement, but deliberately left unfixed there because it is a different write path.

    CREATE TABLE Rd (k INT PRIMARY KEY, v INT,
                     ts TIMESTAMP DEFAULT '2020-01-01 00:00:00' ON UPDATE CURRENT_TIMESTAMP);
    INSERT INTO Rd (k,v) VALUES (1,1);
    INSERT INTO Rd (k,v) VALUES (1,9) ON DUPLICATE KEY UPDATE v = v + 1;

    both report 2 affected, but:
    SELECT ts = '2020-01-01 00:00:00'   mysql 0 (it moved the timestamp)   engine 1 (it did not)

restamp_on_update is called from the two UPDATE paths and not from the upsert branch.

**Not purely additive.** The ODKU branch decides 'changed' by comparing updated != before, and #0089 established that MySQL decides changed AFTER the restamp -- a row whose only difference is the timestamp MySQL moved still counts as changed. So a fix has to order the restamp and the comparison the way the UPDATE paths now do, or it will fix the timestamp and break the count.

Note this cannot be covered in the differential corpus until it is fixed: a case would fail against the empty baseline. It is uncovered until then, which is the argument for doing it soon rather than later.

Relevant to the target application: ON DUPLICATE KEY UPDATE appears 14 times in application-mono-repo, and Prisma upserts compile to it.
