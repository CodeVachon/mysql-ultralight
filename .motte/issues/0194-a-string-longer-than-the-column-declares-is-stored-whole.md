---
id: 194
title: A string longer than the column declares is stored whole
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T15:50:32Z
updated: 2026-09-03T18:11:19Z
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

## Notes

### 2026-09-03T18:11:19Z — Christopher Vachon (user)

Landed in 780f620, gate green (including the Prisma example and migration tests -- no example data had to change, nothing they write overflows a column). Differential 4088 statements, 0 divergences.

Width is CHARACTERS for VARCHAR/CHAR and BYTES for TEXT/TINYTEXT. Trailing spaces are exempt: 'ab   ' into VARCHAR(3) stores 'ab ' with note 1265, while 'abcd ' is still 1406 because the non-space content is too wide. VARBINARY has no such exemption. CHAR strips trailing spaces on read with no warning.

NOT fixed, deliberately, and now recorded in a test rather than left to be rediscovered: the recursive-CTE example in this issue's description. 8.3 refuses SELECT 'a' AS x UNION ALL SELECT CONCAT(x,'b') because its anchor is varchar(1); this engine declares every computed string varchar(512) and has no expression-width arithmetic to refuse with. Enforcing that invented 512 would truncate real rows, which is exactly why the write path uses store() and the read paths still use coerce().
