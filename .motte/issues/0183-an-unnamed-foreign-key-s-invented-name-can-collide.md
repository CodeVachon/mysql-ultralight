---
id: 183
title: An unnamed foreign key's invented name can collide
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T12:30:07Z
updated: 2026-09-03T12:30:07Z
---

## Description

Found while implementing #0133, pre-existing (ddl.rs:917).

An unnamed foreign key is named {table}_ibfk_{len+1}, counting the constraints present rather than looking at the numbers already used. Add two, drop _ibfk_1, add a third: _ibfk_2 is minted twice.

Already reachable before #0133 via DROP FOREIGN KEY. It is the same flaw #0133 fixed for check constraints, where the fix was to count past the highest suffix present rather than the number of entries -- which is what 8.3 does. Same shape, same fix.
