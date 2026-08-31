---
id: 141
title: A hex or bit literal is not read as a number
state: Todo
parent: 9
labels: [executor]
created: 2026-08-31T20:28:05Z
updated: 2026-08-31T20:28:05Z
---

## Description

Found while implementing #0130, where it is the reason BIN() over a binary string has to refuse.

    SELECT X'C3A9' + 0   mysql 50089   engine 0
    SELECT b'101'        mysql 5       engine unsupported

MySQL reads a hex or bit literal as a number in a numeric context and as bytes in a string context. This engine has only the bytes reading, so the numeric one answers 0.

The consequence recorded in #0130: CONV/BIN special-case a hex literal and take its number, while the same bytes in a VARBINARY column are parsed as digits -- BIN(X'C3A9') is 1100001110101001 and BIN(varbinary_col) is 0. Both are Value::Bytes here, so neither answer can be given honestly and BIN over bytes raises 1235. Closing this issue is what lets that refusal become an answer.

Note b'101' does not parse at all, which is a separate parser gap in the same area.
