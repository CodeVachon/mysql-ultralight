---
id: 58
title: describe accepts a column reference execute rejects
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T20:16:48Z
updated: 2026-08-20T20:16:48Z
---

## Description

SELECT a.b FROM J2, where no table or alias `a` is in scope, is refused by MySQL at *prepare* time with 1054 "Unknown column 'a.b' in 'field list'". This engine prepares it happily, reports one column named `b`, and only fails when the statement is executed.

Execution is right; it is the describe path that does not resolve the qualifier. A client that prepares its statements up front — which is what the binary protocol is for — learns about the mistake later than it should, and a test that only prepares learns nothing at all.

Found while measuring column names for #0043.
