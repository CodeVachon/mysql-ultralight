---
id: 131
title: A row predating an ALTER that adds a scale is re-rendered
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-26T16:18:44Z
updated: 2026-09-04T21:13:45Z
---

## Description

Found and deliberately left during #0075.

MySQL rewrites rows only when a column's storage width changes (float to double, or back). An ALTER that only changes (M,D) is metadata-only, so an existing row keeps its old bytes and is merely re-rendered:

    ALTER TABLE t MODIFY f FLOAT(10,4)   over a stale 1.234567890123
    mysql   text 1.2346   binary f32:1.2345679
    engine  text 1.2346   binary f32:1.2346

Text agrees; the binary protocol does not. Before eb9d89a the same case was wrong the other way -- text 1.23457 -- so this is a narrower divergence than it replaced, not a new class.

The cause is that reconcile substitutes a decimal at the result boundary and the binary encoder re-reads the number from those digits, so a stale row is rounded on the way out. Matching MySQL means the scale being available inside as_text(), which is the tagged-value design #0075 ruled out on measurement -- it would make f*f eight places where MySQL gives four.

No corpus case. Reachable only by an ALTER that adds or changes a scale over existing rows.

## Notes

### 2026-09-04T21:13:45Z — Christopher Vachon (user)

Landed in 0f78c5e, gate green. The premise HELD, and it is a binary-protocol bug: a FLOAT(M,D) keeps the four bytes it was written with, and MySQL renders those at the declared scale but SENDS the bytes. The engine was reading the number back out of the SHOWN digits, so it put f32:1.2346 on the wire -- a value no INSERT ever wrote and no text query would show.
