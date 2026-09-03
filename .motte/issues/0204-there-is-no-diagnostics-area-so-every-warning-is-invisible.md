---
id: 204
title: There is no diagnostics area, so every warning is invisible
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T17:51:23Z
updated: 2026-09-03T17:51:23Z
---

## Description

Found while implementing #0194/#0195, and it is the natural successor to #0168.

SHOW WARNINGS is 1235. After #0194/#0195 the engine stores what MySQL stores under a non-strict mode -- truncating a string, clamping an integer -- but says nothing about having done it, where 8.3 records 1265 or 1264 as a warning the client can read.

So WHAT is stored now matches; what is REPORTED about it does not. Cases already known to raise a warning on 8.3 and nothing here:
  1265 data truncated (a non-strict over-long write, and a trailing-space trim under any mode)
  1264 out of range (a non-strict clamp)
  1366 incorrect value (#0200)
  1466 identifier cut (#0168)

#0168 filed this as the absence of a warning channel from the alias side. This is the same absence seen from the write side, and it is now reachable on ordinary INSERTs rather than only on an exotic alias, which is why it is priority 2. Whoever takes it should do #0168 in the same pass.
