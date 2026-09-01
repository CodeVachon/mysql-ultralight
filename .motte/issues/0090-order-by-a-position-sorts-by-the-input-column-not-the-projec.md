---
id: 90
title: ORDER BY a position sorts by the input column, not the projection
state: In Progress
parent: 9
labels: [executor]
created: 2026-08-25T05:12:45Z
updated: 2026-09-01T14:31:09Z
---

## Description

Found while measuring for #0061, and nothing to do with booleans -- it reproduces with plain integers.

    SELECT JSON_ARRAY(x) FROM (SELECT 1 AS x UNION SELECT 0) t ORDER BY 1
    mysql   [0], [1]
    engine  [1], [0]

ORDER BY 1 means the first column of the select list, so it should sort by the projected expression. This engine resolves the position against the input relation instead, so it sorts by x where MySQL sorts by JSON_ARRAY(x). ORDER BY on the alias is correct, so only the positional form is wrong.

A wrong answer rather than an error, and a quiet one: the rows are all present and plausibly ordered, so nothing looks broken unless the expected order is known. ORDER BY <position> is common in generated SQL, which is what makes it worth more than its size.

Worth checking the same resolution for GROUP BY <position>, which MySQL resolves the same way, and for a position that exceeds the select list -- MySQL raises 1054.
