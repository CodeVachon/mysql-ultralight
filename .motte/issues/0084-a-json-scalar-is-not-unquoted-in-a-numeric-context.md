---
id: 84
title: A JSON scalar is not unquoted in a numeric context
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-24T20:58:12Z
updated: 2026-09-03T14:20:26Z
---

## Description

Found while checking the Value::Json arm during #0079, and pre-existing rather than caused by it.

A JSON scalar holding a number or a numeric string is read as 0 wherever a number is wanted:

    j holding 2026    SELECT j + 0            mysql 2026   engine 0
                      SELECT j | 0            mysql 2026   engine 0
                      SELECT CAST(j AS SIGNED) mysql 2026  engine 0

Not bitwise-specific -- the arithmetic and cast paths answer the same way -- so it is not #0079's, and it was there before that commit. The document's text still carries its JSON spelling when a numeric context reads it, and nothing unquotes it on the way.

## Notes

### 2026-09-03T14:00:23Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3 with both servers asked by the same client. The premise is HALF STALE -- scope is now smaller than the description says.

Fixed since filing: a JSON *number* in a numeric context.
    j holding 2026:  j+0, j*2, j-1, j|0, CAST(j AS SIGNED), ABS(j), j DIV 2, j>1000  all agree with 8.3 now.

Still broken, and it is the half the title is actually about -- a quoted numeric is not unquoted:
    s holding '"2026"':  s + 0       mysql 2026   engine 0
                           s * 2       mysql 4052   engine 0
                           CAST(s AS SIGNED)  mysql 2026   engine 0

So the remaining work is the quoted-string case only. Anyone taking this should not re-fix the number path; a probe that only tries j+0 over a JSON number will look green and prove nothing.
