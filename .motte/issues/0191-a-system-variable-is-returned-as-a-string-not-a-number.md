---
id: 191
title: A system variable is returned as a string, not a number
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T14:04:38Z
updated: 2026-09-03T14:20:34Z
---

## Description

Found twice while validating Epic 9 premises on 2026-09-03, once under #0095 and once under #0151.

A numeric system variable comes back typed as a string here where 8.3 returns a number:

    SELECT @@group_concat_max_len        8.3 renders it right-aligned (a number), the engine left-aligned (a string)
    SELECT @@group_concat_max_len + 1    mysql LONGLONG   engine DOUBLE
    SELECT @@lower_case_table_names      same shape
    SELECT @@max_execution_time          same shape

The value is right in each case; the type code is not, and arithmetic over one is DOUBLE here where MySQL keeps it integral. A client that reads the column type -- or an ORM that decides how to parse the value from it -- sees a string.

Separate from #0095, which is about SET refusing and clamping the values on the way in. This is about reading them back out.
