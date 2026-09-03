---
id: 203
title: AUTO_INCREMENT overflow answers 1264 instead of clamping to 1062
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T17:51:23Z
updated: 2026-09-03T17:51:23Z
---

## Description

Found while implementing #0194/#0195, measured on 8.3 under BOTH strict and non-strict modes.

When an AUTO_INCREMENT column reaches its type's maximum, MySQL clamps the counter to that maximum and then fails the insert with 1062 duplicate key -- the same answer under either mode. The engine answers 1264 out of range under a strict mode. It agrees under a non-strict one after #0194/#0195.

Also measured at the same time: the duplicate-key message names the key 'PRIMARY' here where 8.3 says 'A.PRIMARY' -- qualified with the table. The same unqualified-key-name divergence was noted independently during the collation work, so it shows up on more than one path.
