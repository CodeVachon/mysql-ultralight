---
id: 43
title: The arrow operators render with spaces in a column name
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T14:21:40Z
updated: 2026-08-20T14:21:40Z
---

## Description

Found by the differential harness. A column named by doc->>'$.b' is called "doc->>'$.b'" by MySQL and "doc ->> '$.b'" here, because the name comes from rendering the parsed expression rather than the text as written.

Cosmetic, but a column name is what a client keys a row by, so a test reading the column by name gets nothing.
