---
id: 187
title: information_schema VIEW_DEFINITION is not the text MySQL stores
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T12:52:36Z
updated: 2026-09-03T14:20:33Z
---

## Description

Found while implementing #0094, pre-existing and true of every view rather than only the ones that change.

    engine  SELECT EXTRACT(DAY_SECOND FROM dt) AS a FROM X
    mysql   select extract(day_second from `m`.`X`.`dt`) AS `a` from `m`.`X`

MySQL stores its own normalised rendering: lower-cased keywords, every name fully qualified with the schema, and everything backquoted. The engine stores something close to what was written.

A tool that reads VIEW_DEFINITION to diff or re-create a view sees different text here than against a real server. Not in the corpus. Noticed while making sure an internal rewrite could not leak into that column.
