---
id: 260
title: An unnamed foreign key is numbered past the named ones
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-08T17:13:24Z
updated: 2026-09-08T17:13:24Z
---

## Description

Found on 2026-09-08 while adding coverage for #0257, with scripts/ask.sh. Pre-existing and separate from that ticket.

MySQL names an unnamed foreign key `<table>_ibfk_<N>` where N counts only the keys it has named that way. This engine uses `table.foreign_keys.len() + 1`, which counts explicitly named keys too, so the first auto-named key on a table that already has a named one comes out one too high.

    CREATE TABLE T (id INT NOT NULL PRIMARY KEY, a INT, b INT, c INT);
    ALTER TABLE T ADD CONSTRAINT named1 FOREIGN KEY (a) REFERENCES P (id);
    ALTER TABLE T ADD FOREIGN KEY (b) REFERENCES P (id);
      mysql   named1, T_ibfk_1
      engine  named1, T_ibfk_2
    ALTER TABLE T ADD FOREIGN KEY (c) REFERENCES P (id);
      mysql   named1, T_ibfk_1, T_ibfk_2
      engine  named1, T_ibfk_2, T_ibfk_3

It shows up as more than a cosmetic name now that #0257 refuses a duplicate constraint name: a statement that would collide with `T_ibfk_1` on 8.3 does not collide here, so the two servers disagree about whether the statement is legal at all. That is how this was found -- a differential case asserting the collision passed on MySQL and not here.

WHAT IS NOT MEASURED, and should be before implementing: what MySQL does after a *drop*. Whether `_ibfk_` numbering is "count of auto-named keys + 1" or "highest existing number + 1" is only decidable by dropping `T_ibfk_1` and adding another, and both rules fit the measurements above. Do not implement one on the strength of the other.

Priority 3: Prisma names every constraint it creates, so the target application never reaches an auto-generated name.
