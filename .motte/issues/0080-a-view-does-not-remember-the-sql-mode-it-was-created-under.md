---
id: 80
title: A view does not remember the sql_mode it was created under
state: Todo
parent: 9
labels: [compat]
created: 2026-08-24T17:36:23Z
updated: 2026-09-03T14:05:52Z
---

## Description

Found by review during #0053, and pre-existing -- #0053 made a third flag visible through it rather than causing it.

MySQL freezes the creating session's sql_mode into a view, so a view's body always means what it meant when it was written. This engine re-resolves a view's body against the *reader's* mode, so an execution-time flag changes a stored view's answers:

    PIPES_AS_CONCAT, view of 'a' || 'b'   created under the flag: ab, then 0 once cleared   mysql ab both times
    REAL_AS_FLOAT, view of CAST(x AS REAL)   Float/1.23457 under the flag, Double/1.234567890123 after

ANSI_QUOTES and IGNORE_SPACE are unaffected: those reach the code by rewriting the text before parsing, and the rewrite is frozen at CREATE VIEW. The flags that reach the code through the per-statement Origin -- PIPES_AS_CONCAT, and now REAL_AS_FLOAT -- are not.

Fixing it means storing the mode with the view and resolving the body under it. crates/engine/src/image.rs is append-only, so the field is a new FORMAT_VERSION, and every view execution path has to route the stored mode rather than the session's.

Only visible when a session changes sql_mode mid-connection. Under a fixed server-wide mode, which is how this engine is used, nothing diverges.

## Notes

### 2026-08-24T18:44:02Z — Christopher Vachon (user)

Confirmed again by review during #0081, and #0053 widened it: REAL inside a view body now resolves under the reading session's mode, so one view gives Float/1.23457 while REAL_AS_FLOAT is set and Double/1.234567890123 after it is cleared, where MySQL gives float both times. Before #0053 a REAL in a view was unconditionally double -- wrong against MySQL under the flag, but at least self-consistent between two reads.

Narrow: only a REAL written inside a view body. A view over a REAL column is fine, because the column's type is baked at DDL time.

### 2026-09-03T14:05:52Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. A view of 'a' || 'b' created under PIPES_AS_CONCAT answers 'ab' on 8.3 after the mode is cleared, and 0 here -- the view is re-parsed under the session's current mode instead of the one it was created under.
