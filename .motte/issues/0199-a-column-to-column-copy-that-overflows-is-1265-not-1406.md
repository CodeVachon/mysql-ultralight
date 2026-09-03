---
id: 199
title: A column-to-column copy that overflows is 1265, not 1406
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T17:51:05Z
updated: 2026-09-03T17:51:05Z
---

## Description

Found while implementing #0194/#0195, measured on 8.3.

    INSERT INTO V SELECT s FROM Src                        mysql 1265 (01000)
    CREATE TABLE t (v VARCHAR(3)) AS SELECT v FROM src     mysql 1265 (01000)
    INSERT INTO V SELECT CONCAT(s,'') FROM Src             mysql 1406
    ...a literal, or a subquery                            mysql 1406
    engine says 1406 for all of them

So MySQL distinguishes a bare column-to-column copy from anything computed. Both refuse the write, so this is a wrong code rather than a wrong answer, which is why it was left rather than guessed at. Deliberately not in the corpus.

Note 1265 is SQLSTATE 01000, a warning class, even though it refuses under a strict mode.
