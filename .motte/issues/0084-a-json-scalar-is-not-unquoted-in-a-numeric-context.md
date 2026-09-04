---
id: 84
title: A JSON scalar is not unquoted in a numeric context
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-24T20:58:12Z
updated: 2026-09-03T22:31:37Z
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

### 2026-09-03T22:31:36Z — Christopher Vachon (user)

Landed in f9904f1, gate green. Differential 4280 statements, 0 divergences.

Scope was correctly the smaller one recorded in the earlier validation note: the JSON *number* path was already fixed, so only the quoted case was done. The test covers the number path too, so it cannot regress.

The conversion rule, measured: a numeric context reads the scalar itself -- a string reads as that SQL string (' 2026 '->2026, '+2026'->2026, '2.5e3'->2500, '12abc'->12, '0x10'->0, 'abc'->0), a boolean is 1/0, anything else reads as its rendering. CAST to SIGNED/UNSIGNED takes the LEADING-INTEGER reading instead, so '1e30' is 1 there where arithmetic gives the whole number -- two different rules on the same value.
