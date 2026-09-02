---
id: 154
title: A literal's case fold uses the wrong collation
state: Todo
parent: 9
labels: [compat]
created: 2026-09-02T17:25:27Z
updated: 2026-09-02T17:25:27Z
---

## Description

Found by the full-codepoint sweep in #0140.

The case-folding table belongs to the COLLATION, not the character set, and utf8mb4 has two on 8.3 that disagree on 616 characters upward and 608 downward:

    utf8mb4_0900_ai_ci   UPPER changes 1321 chars, folds above the BMP
    utf8mb4_general_ci   UPPER changes 706 chars, never folds above the BMP
    HEX(UPPER('ȿ'))      '' under 0900_ai_ci, C8BF under general_ci
    HEX(LOWER('ẞ'))      C39F under 0900_ai_ci, E1BA9E under general_ci

A column declared without COLLATE gets 0900_ai_ci; a literal takes the CONNECTION's collation, which mysql2 and the differential harness's client both negotiate as 45 = general_ci. So 8.3 answers SELECT UPPER('𐐨') and SELECT UPPER(col) differently for the same character.

#0140 encoded 0900_ai_ci, which is right for a column and wrong for a literal:

    SELECT UPPER('𐐨')     engine F0909080   8.3 F09090A8
    SELECT UPPER('aȿb')   engine 41E2B1BE   8.3 61C8BF62

The corpus asks those characters only through a column, with the reason recorded.

Closing this needs the engine to know a literal's collation is the connection's, which is #0129/#0152 territory -- the same missing mechanism. Note it also means the answer depends on how the client connected, so a corpus case must pin the connection collation or it will drift.
