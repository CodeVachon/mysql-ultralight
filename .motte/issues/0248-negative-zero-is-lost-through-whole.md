---
id: 248
title: Negative zero is lost through whole()
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-04T09:47:08Z
updated: 2026-09-04T09:47:08Z
---

## Description

Found while implementing #0223, shared by three functions.

    SELECT ROUND(-0.4e0), CEIL(-0.4e0), TRUNCATE(-0.4e0, 0)
    mysql   -0  -0  -0
    engine   0   0   0

Visible as characters, not just as a value: CONCAT(ROUND(-0.5e0)) is '-0' on 8.3.

All three go through whole(), which drops the sign when the magnitude rounds to zero. IEEE 754 keeps it and MySQL prints it.
