---
id: 162
title: LAST_INSERT_ID(expr) ignores its argument
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-03T03:40:02Z
updated: 2026-09-03T14:20:31Z
---

## Description

Found while implementing #0132, which fixed the no-argument form.

    SELECT LAST_INSERT_ID(42)    mysql returns 42 and sets the session value to 42
    SELECT LAST_INSERT_ID(NULL)  mysql returns NULL and sets 0
    SELECT LAST_INSERT_ID(-1)    mysql returns 18446744073709551615
    engine returns the session value in every case, ignoring the argument

A wrong answer, and it also silently drops the write half -- a later bare LAST_INSERT_ID() does not see what the client set.

The one-argument form exists so a client can carry its own id through a connection, which is exactly the pattern a test fixture uses, so this is more likely to matter here than its obscurity suggests.

#0132 left it because refusing or implementing it properly means touching the resolve-time refusal sites -- the #0009 zero-rows pattern -- which is its own change rather than a rider on the reporting fix.

## Notes

### 2026-09-03T14:01:19Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. LAST_INSERT_ID(42) mysql 42 engine 0; the following LAST_INSERT_ID() mysql 42 engine 0; LAST_INSERT_ID(-1) mysql 18446744073709551615 engine empty.
