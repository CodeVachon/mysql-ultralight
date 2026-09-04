---
id: 225
title: A cast of invalid bytes to a character type is not NULL
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T04:08:09Z
updated: 2026-09-04T04:08:09Z
---

## Description

Found while implementing #0215, independent of the declared width.

    SELECT CAST(UNHEX('FFEEDD') AS CHAR) IS NULL, CAST(UNHEX('FFEEDD') AS CHAR(3)) IS NULL
    mysql   1  1   plus Warning 1300 Invalid utf8mb4 character string: 'FFEEDD'
    engine  0  0   -- replacement characters instead

So the engine answers a string where MySQL answers NULL. Same lossy-decode root as #0145 and the LENGTH issue filed beside this one, but a different consequence: not a wrong length, a wrong value and a wrong NULL-ness.

The diagnostics area from #0204 exists now, so 1300 has somewhere to go.
