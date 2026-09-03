---
id: 101
title: A qualified name with no FROM is 1054 where MySQL says 1109
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-08-25T14:48:58Z
updated: 2026-09-03T14:20:27Z
---

## Description

Found while implementing #0058.

    SELECT a.b            mysql 1109 42S02 Unknown table 'a' in field list   engine 1054
    SELECT 1 WHERE a.b=1  mysql 1109 Unknown table 'a' in where clause       engine answers one row

Both errors, so the first is a wrong code rather than a wrong answer -- but the second answers where MySQL refuses. Note MySQL writes the clause without quotes in 1109 (in field list) where 1054 quotes it (in 'field list'), which is easy to get wrong when copying one from the other.

## Notes

### 2026-09-03T14:00:24Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client both sides. HALF ALREADY FIXED.

    SELECT a.b            mysql 1109 42S02 Unknown table 'a' in field list
                          engine 1054 42S22 Unknown column 'a.b' in 'field list'   -- still wrong
    SELECT 1 WHERE a.b=1  mysql 1109 ... in where clause
                          engine 1054 ... in 'where clause'                        -- now REFUSES

The description says the second 'answers one row'. It no longer does -- it refuses, with the wrong code. So this is now purely a wrong-code issue and not a wrong-answer one, which lowers its severity.

Confirmed too: 1109 writes the clause unquoted (in field list) where 1054 quotes it (in 'field list').
