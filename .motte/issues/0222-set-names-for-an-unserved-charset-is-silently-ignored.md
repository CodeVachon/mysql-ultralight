---
id: 222
title: SET NAMES for an unserved charset is silently ignored
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T02:36:38Z
updated: 2026-09-04T02:36:38Z
---

## Description

Found while implementing #0154, which taught the engine to honour SET NAMES for the charsets it serves.

    SET NAMES latin1
    mysql   @@collation_connection = latin1_swedish_ci, @@character_set_connection = latin1
    engine  keeps utf8mb4_0900_ai_ci / utf8mb4, silently

Pre-existing, and deliberately left: refusing it would reject a connection the engine currently serves, and honouring it means modelling latin1, which is #0129's unfinished half (the engine still has one character set).

Recording it because after #0154 the engine DOES honour SET NAMES for utf8mb4, so the silent no-op for everything else is now the inconsistent case rather than uniform behaviour.
