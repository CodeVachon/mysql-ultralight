---
id: 120
title: CAST to an integer truncates where MySQL rounds
state: Todo
parent: 9
labels: [executor]
created: 2026-08-26T00:18:26Z
updated: 2026-08-26T00:18:26Z
---

## Description

Found by review during #0070, and pre-existing -- the same answer at HEAD for both literals and DECIMAL columns.

    SELECT CAST(1.5 AS SIGNED)    mysql 2   engine 1
    SELECT CAST(2.5 AS SIGNED)    mysql 3   engine 2

MySQL rounds half away from zero; this engine truncates toward zero.

Worth doing now because #0070 made it inconsistent with its neighbours: decimal::units already implements half-away-from-zero exactly, and #0079 measured and encoded the same rule for a bitwise operand, so the cast is the one numeric reader still truncating. Check CAST(... AS UNSIGNED) and the negative cases in the same pass, and whether an approximate operand (a DOUBLE) rounds the same way an exact one does.
