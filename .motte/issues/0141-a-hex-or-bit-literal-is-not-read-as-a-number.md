---
id: 141
title: A hex or bit literal is not read as a number
state: Todo
parent: 9
labels: [executor]
created: 2026-08-31T20:28:05Z
updated: 2026-09-03T14:00:44Z
---

## Description

Found while implementing #0130, where it is the reason BIN() over a binary string has to refuse.

    SELECT X'C3A9' + 0   mysql 50089   engine 0
    SELECT b'101'        mysql 5       engine unsupported

MySQL reads a hex or bit literal as a number in a numeric context and as bytes in a string context. This engine has only the bytes reading, so the numeric one answers 0.

The consequence recorded in #0130: CONV/BIN special-case a hex literal and take its number, while the same bytes in a VARBINARY column are parsed as digits -- BIN(X'C3A9') is 1100001110101001 and BIN(varbinary_col) is 0. Both are Value::Bytes here, so neither answer can be given honestly and BIN over bytes raises 1235. Closing this issue is what lets that refusal become an answer.

Note b'101' does not parse at all, which is a separate parser gap in the same area.

## Notes

### 2026-09-03T14:00:44Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3. The X'' half holds. **The b'' half of the description is WRONG and would mislead an implementer.**

The description says `SELECT b'101'` is 5 on MySQL. It is not -- a bit literal is a *binary string*, and 5 is only what a numeric context makes of it:

    SELECT b'101'                     mysql returns the byte 0x05 (prints blank), NOT 5
    SELECT HEX(b'101')                mysql 05
    SELECT b'101' + 0                 mysql 5
    SELECT CAST(b'101' AS UNSIGNED)   mysql 5

Exactly the same shape as the hex literal it sits beside, which the description gets right:

    SELECT X'C3A9'      mysql 'é'      SELECT X'C3A9' + 0   mysql 50089   engine 0
    SELECT 0xC3A9 + 0   mysql 50089    engine 0

Engine today: X'C3A9' + 0 is 0, and b'101' is refused outright with 1235.

So the fix is 'a bit or hex literal is a binary string that reads as a number in a numeric context', NOT 'b'101' evaluates to 5'. Implementing the description literally would make SELECT b'101' answer 5 where MySQL answers a byte.
