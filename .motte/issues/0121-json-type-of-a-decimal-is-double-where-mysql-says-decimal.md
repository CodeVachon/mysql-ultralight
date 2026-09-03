---
id: 121
title: JSON_TYPE of a decimal is DOUBLE where MySQL says DECIMAL
state: Todo
parent: 9
labels: [compat]
created: 2026-08-26T03:49:12Z
updated: 2026-09-03T14:01:18Z
---

## Description

Found while implementing #0071, and left there with the measurement that settles why it needs more than that change.

    JSON_TYPE(JSON_EXTRACT(JSON_OBJECT('a', d), '$.a'))  over DECIMAL(10,2) 2.50
    mysql DECIMAL   engine DOUBLE

#0071 made every number in a document hold the text it will be written as, which looked like enough to answer this. It is not, and MySQL's own rendering is the proof:

    DECIMAL(10,1) 2.5    renders 2.5   types DECIMAL
    CAST('2.5' AS JSON)  renders 2.5   types DOUBLE
    DECIMAL(10,0) 2      renders 2     types DECIMAL
    CAST('2' AS JSON)    renders 2     types INTEGER

Byte-identical renderings, different types, so the spelling is provably not a function of the type. The only partial rule available -- trailing zeros past the shortest form, which no double spells -- would answer DECIMAL for a DECIMAL(10,2) 2.50 and DOUBLE for a DECIMAL(10,1) 2.5, making the answer depend on the column's scale where MySQL says DECIMAL for both. That is a rule MySQL does not have, so #0071 did not invent one.

MySQL carries the type in its binary document format rather than in the text. Closing this means a Value::Json that carries the type beside the text, which reaches the append-only image format and so needs a FORMAT_VERSION bump. The four measured rows and this reasoning are in a comment at functions.rs's json_type arm.

## Notes

### 2026-09-03T14:01:18Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. JSON_TYPE over JSON_OBJECT of DECIMAL(10,2) and DECIMAL(10,1): mysql DECIMAL, engine DOUBLE. DECIMAL(10,0): mysql DECIMAL, engine INTEGER. CAST('2' AS JSON) agrees at INTEGER.
