---
id: 90
title: ORDER BY a position sorts by the input column, not the projection
state: Done
parent: 9
labels: [executor]
created: 2026-08-25T05:12:45Z
updated: 2026-09-01T20:08:23Z
---

## Description

Found while measuring for #0061, and nothing to do with booleans -- it reproduces with plain integers.

    SELECT JSON_ARRAY(x) FROM (SELECT 1 AS x UNION SELECT 0) t ORDER BY 1
    mysql   [0], [1]
    engine  [1], [0]

ORDER BY 1 means the first column of the select list, so it should sort by the projected expression. This engine resolves the position against the input relation instead, so it sorts by x where MySQL sorts by JSON_ARRAY(x). ORDER BY on the alias is correct, so only the positional form is wrong.

A wrong answer rather than an error, and a quiet one: the rows are all present and plausibly ordered, so nothing looks broken unless the expected order is known. ORDER BY <position> is common in generated SQL, which is what makes it worth more than its size.

Worth checking the same resolution for GROUP BY <position>, which MySQL resolves the same way, and for a position that exceeds the select list -- MySQL raises 1054.

## Notes

### 2026-09-01T20:08:23Z — Christopher Vachon (user)

Done in 613ae1d, swept with #0144 as one mechanism -- sort_rows resolving against the relation because the sort runs before the projection exists.

A position is now always a select-list position, counted over the expanded list, with 1054 for 0 or out of range. GROUP BY <position> follows the same rule under 'group statement', after the field list.

The measured discriminator is the item's SHAPE, not what happens to resolve: a bare integer is a position -- ORDER BY (2) is column 2 after stripping parens -- while ORDER BY 1+1, 1.0 and '1' are constants that order nothing.
