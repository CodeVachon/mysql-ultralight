---
id: 200
title: Non-numeric text into a numeric column is refused under every mode
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T17:51:08Z
updated: 2026-09-03T17:51:08Z
---

## Description

Found while implementing #0194/#0195. Measured on 8.3 with sql_mode='':

    INSERT INTO N (i) VALUES ('abc')     mysql stores 0, warning 1366
    INSERT INTO N (i) VALUES ('12abc')   mysql stores 12, warning 1265
    engine refuses under EVERY mode

#0194/#0195 taught the engine what a strict mode means for width and range, but not for a value that is not a number at all. It is documented as a deliberate remaining gap on SqlMode::strict.

Related message-text divergence measured at the same time: 8.3 says 'Incorrect integer value: ''abc'' for column ''i'' at row 1' where the engine says 'Incorrect int value for column ''i''' -- no value, no row, and the type spelled differently.
