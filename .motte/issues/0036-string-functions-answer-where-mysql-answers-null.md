---
id: 36
title: String functions answer '' where MySQL answers NULL
state: Todo
parent: 9
labels: [compat, executor]
created: 2026-08-20T14:21:10Z
updated: 2026-08-20T14:21:10Z
---

## Description

Found by the differential harness. UPPER, LOWER, TRIM, SUBSTRING, REPLACE, LEFT and RIGHT return an empty string for a NULL argument, and LENGTH returns 0. MySQL returns NULL for all of them.

This is the worst class the harness found, because it is a wrong answer rather than wrong metadata: WHERE UPPER(t) = '' matches rows here that MySQL would not return, and a test asserting NULL gets an empty string instead.

Evidence: SELECT CONCAT(t,'!'), UPPER(t), LOWER(t), LENGTH(t) FROM S over a row where t IS NULL gives NULL, NULL, NULL, NULL from MySQL and NULL, '', '', '0' here. CONCAT alone propagates correctly, which is why this went unnoticed.
