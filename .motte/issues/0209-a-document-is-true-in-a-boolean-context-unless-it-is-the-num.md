---
id: 209
title: A document is true in a boolean context unless it is the number 0
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T22:31:37Z
updated: 2026-09-03T22:31:37Z
---

## Description

Found while implementing #0084/#0121, measured on 8.3 over 14 documents.

    WHERE j / j IS TRUE / NOT j / j AND 1
    8.3 is TRUE for every document except the JSON number 0 -- including '"abc"', 'null', 'false', '""', '"0"', '[]' and '{}'
    the engine uses the numeric reading, so all of those are false

The catch, and why this is not a one-line fix: IF(j,1,0) on 8.3 uses the NUMERIC reading and already agrees with the engine for all 14 documents. So one Value::truthy cannot serve both contexts -- a predicate and IF() read the same document differently.
