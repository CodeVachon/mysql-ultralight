---
id: 101
title: A qualified name with no FROM is 1054 where MySQL says 1109
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T14:48:58Z
updated: 2026-08-25T14:48:58Z
---

## Description

Found while implementing #0058.

    SELECT a.b            mysql 1109 42S02 Unknown table 'a' in field list   engine 1054
    SELECT 1 WHERE a.b=1  mysql 1109 Unknown table 'a' in where clause       engine answers one row

Both errors, so the first is a wrong code rather than a wrong answer -- but the second answers where MySQL refuses. Note MySQL writes the clause without quotes in 1109 (in field list) where 1054 quotes it (in 'field list'), which is easy to get wrong when copying one from the other.
