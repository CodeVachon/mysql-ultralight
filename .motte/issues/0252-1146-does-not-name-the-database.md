---
id: 252
title: 1146 does not name the database
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T21:52:04Z
updated: 2026-09-04T21:52:04Z
---

## Description

Measured while implementing #0244, and now visible at prepare time as well as at execute.

    SELECT * FROM NoSuch
    mysql   1146 Table 'pp.NoSuch' doesn't exist
    engine  1146 Table 'NoSuch' doesn't exist

The engine is inconsistent with itself since #0232: RENAME TABLE's 1146 DOES qualify the name (it was measured that way -- 'Table ''probe.nosuch'' doesn't exist') while every other site does not. So the same code carries two spellings depending on which statement raised it.

EngineError::no_such_table takes a bare name and has 13-ish call sites, each of which has the database in hand at the point it raises. Qualifying them all is mechanical but touches many messages at once, so it wants its own differential run rather than riding along with another change.
