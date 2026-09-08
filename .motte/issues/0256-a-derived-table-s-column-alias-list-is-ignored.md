---
id: 256
title: A derived table's column alias list is ignored
state: Done
parent: 9
labels: [executor, app-priority-1]
created: 2026-09-08T14:26:16Z
updated: 2026-09-08T15:44:01Z
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

## Notes

### 2026-09-08T15:44:01Z — Christopher Vachon (user)

FIXED in 479ae91. With #0255 before it, ALL 604 MIGRATIONS OF THE REAL PRISMA HISTORY NOW APPLY, and a second `migrate deploy` against the result answers "No pending migrations to apply". 278 tables, 2771 columns.

The rule is one function for both spellings now. Measured on 8.3, each line:

  * a list of the wrong length, either way, is 1353 -- and its message carries no name on the end, where this engine appended `: 'd'`
  * a list that repeats a name is 1060, even when nothing selects that column
  * names map by position, and the original name stops resolving (1054)
  * a list after anything but a derived table's alias -- a base table, a CTE reference, JSON_TABLE -- is 1064, and was being IGNORED here, answering rows with the columns still under their old names
  * so is a list that gives a column a type

The last two came from review and are the house rule rather than the ticket: a gap has to be a clear error and not a wrong answer. Both list errors are also refused at prepare now, because how many names a list holds and whether it repeats one are settled by the text -- 8.3 fails the PREPARE where this engine described zero columns and let the client find out at execute. Same shape as #0244, and Prisma prepares everything.

ONE REVIEW FINDING CHANGED ON MEASUREMENT rather than being confirmed. 8.3 checks a DERIVED TABLE's list before running its subquery -- 1353 beats a 1690 raised inside it -- and checks a CTE's list AFTER, where 1690 wins. The engine matches 8.3 on the CTE and diverges on the derived table. The two therefore cannot be made consistent with each other without breaking the half that is already right, which is why it is #0258 and not a line in this change.

Differential: 5608 statements, 0 divergences, with a group covering every line above.
