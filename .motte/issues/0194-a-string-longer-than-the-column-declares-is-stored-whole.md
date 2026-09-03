---
id: 194
title: A string longer than the column declares is stored whole
state: In Progress
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T15:50:32Z
updated: 2026-09-03T16:32:40Z
---

## Description

Found while implementing #0192 and confirmed with no CTE involved.

    CREATE TABLE Narrow (id INT, s VARCHAR(3));
    INSERT INTO Narrow VALUES (1, 'abcdef');
    SELECT s FROM Narrow
    mysql   1406 (22001) Data too long for column 's' at row 1
    engine  stores and returns 'abcdef'

SqlType::coerce does not enforce a string column's declared length, so the engine holds a row MySQL would have refused to write. A migration or a seed that overflows a column passes here and fails against a real server -- the failure this engine exists to prevent.

The target application declares 2359 VARCHAR columns, most of them VARCHAR(191), so an over-long write is an ordinary mistake to make.

Through a recursive CTE it also produces wrong rows rather than an error: SELECT 'a' AS x UNION ALL SELECT CONCAT(x,'b') answers 5 rows here where 8.3 is 1406, because the anchor's varchar(1) is not enforced.
