---
id: 84
title: A JSON scalar is not unquoted in a numeric context
state: Todo
parent: 9
labels: [compat]
created: 2026-08-24T20:58:12Z
updated: 2026-08-24T20:58:12Z
---

## Description

Found while checking the Value::Json arm during #0079, and pre-existing rather than caused by it.

A JSON scalar holding a number or a numeric string is read as 0 wherever a number is wanted:

    j holding 2026    SELECT j + 0            mysql 2026   engine 0
                      SELECT j | 0            mysql 2026   engine 0
                      SELECT CAST(j AS SIGNED) mysql 2026  engine 0

Not bitwise-specific -- the arithmetic and cast paths answer the same way -- so it is not #0079's, and it was there before that commit. The document's text still carries its JSON spelling when a numeric context reads it, and nothing unquotes it on the way.
