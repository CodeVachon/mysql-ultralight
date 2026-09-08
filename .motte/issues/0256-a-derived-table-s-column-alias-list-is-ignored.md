---
id: 256
title: A derived table's column alias list is ignored
state: Todo
parent: 9
labels: [executor, app-priority-1]
created: 2026-09-08T14:26:16Z
updated: 2026-09-08T14:26:16Z
---

## Description

BLOCKS THE TARGET APPLICATION, at migration 407 of 604 (`20250910202217_update_user_notification_settings`) once #0255 is fixed.

    Database error code: 1054
    Unknown column 'enum_values.type' in 'field list'

The migration uses MySQL 8.0.19's table value constructor with a column alias:

    SELECT ..., enum_values.type, ...
    FROM User u
      CROSS JOIN (VALUES ROW('ACKNOWLEDGE_FEEDBACK'), ROW('TIMESHEET_APPROVED'), ...)
        AS enum_values(type)
    WHERE ...

MEASURED ON 8.3, 2026-09-08, with scripts/ask.sh. The gap is not the table value constructor -- that works -- it is the `(col, ...)` alias list, and it is missing for derived tables generally:

  SELECT * FROM (VALUES ROW(1), ROW(2)) AS t     both agree, column is `column_0`
  SELECT * FROM (VALUES ROW(1), ROW(2)) AS t(x)  8.3 names it `x`; engine keeps `column_0`
  SELECT t.x  FROM (VALUES ROW(1), ROW(2)) AS t(x)   8.3 answers; engine 1054
  SELECT * FROM (SELECT 1 AS a) AS d(x)          8.3 names it `x`; engine keeps `a`
  SELECT d.x  FROM (SELECT 1 AS a) AS d(x)       8.3 answers; engine errors

So `VALUES ROW(...)` is already supported and the fix is to apply an alias's column list to the derived table's output names -- positionally, and refusing a list whose length does not match the number of columns, which is what 8.3 does.

Priority 1: the target application's migration history stops here.
