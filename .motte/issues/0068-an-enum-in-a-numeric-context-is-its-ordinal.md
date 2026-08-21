---
id: 68
title: An ENUM in a numeric context is its ordinal
state: Todo
parent: 9
labels: [executor]
created: 2026-08-21T13:36:26Z
updated: 2026-08-21T13:37:06Z
---

## Description

MySQL gives an ENUM a number: its 1-based position in the declared list. This engine stores the member as text and reads the number out of that text, so a member that is not itself numeric reads as 0.

    ENUM('a','b') holding 'a'          mysql   engine
    SELECT e + 0                            1        0
    SELECT -e                              -1       -0
    SELECT e = 1                            1        0
    SELECT CAST(e AS SIGNED)                1        0
    SELECT e = 'a'                          1        1

Only the comparison against the *member* behaves; every numeric reading is wrong, comparison included.

Found while fixing #0063, where unary minus started reading its operand as a number instead of answering NULL. That made the divergence visible rather than causing it -- before, these answered NULL, which was wrong too.

The same shape as #0054, where a temporal loses its type on the way into arithmetic: crates/engine/src/value.rs holds only the text, and the answer depends on the column's declared type. SqlType already carries the variants in declaration order, so the ordinal is known -- it is the value that cannot reach it.
