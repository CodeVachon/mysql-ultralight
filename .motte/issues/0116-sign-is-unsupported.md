---
id: 116
title: SIGN is unsupported
state: In Progress
parent: 9
labels: [executor]
created: 2026-08-25T23:13:29Z
updated: 2026-09-03T01:51:05Z
---

## Description

`SELECT SIGN(1.50)` is 1235 `mysql-ultralight does not yet support function \`sign()\``. MySQL answers -1, 0 or 1 as a LONGLONG.

A clear refusal rather than a wrong answer, so it is a gap and not a bug. Found while sweeping the functions that touch a decimal for #0070 — the rest of that sweep (ABS, ROUND, TRUNCATE, CEILING, FLOOR, COALESCE, IFNULL, IF, MIN, MAX, SUM, MOD) either agreed already or was fixed there.
