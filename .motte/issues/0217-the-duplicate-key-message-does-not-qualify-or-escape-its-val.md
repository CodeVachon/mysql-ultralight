---
id: 217
title: The duplicate-key message does not qualify or escape its value
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T01:36:12Z
updated: 2026-09-04T01:36:12Z
---

## Description

Two halves of the 1062 message, both measured on 8.3 and both pre-existing. Found independently by three separate pieces of work (#0150's index naming, the collation cluster, and #0201).

    key name    mysql 'b1.ub' (qualified with the table)   engine 'ub'
    raw bytes   mysql 'a\x00\x00' (escaped)               engine emits the NULs raw

The second matters more than it looks: a client reading the message as a C string truncates it at the first NUL, so it prints 'Duplicate entry ''a' and loses the key name entirely. #0201 closed this for BINARY columns by rendering the value through the column's type, but VARBINARY holding embedded NULs still emits them raw.

Both halves live in catalog.rs::check_unique.
