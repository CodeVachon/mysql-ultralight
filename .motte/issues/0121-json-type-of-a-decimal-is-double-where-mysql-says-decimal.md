---
id: 121
title: JSON_TYPE of a decimal is DOUBLE where MySQL says DECIMAL
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-26T03:49:12Z
updated: 2026-09-03T22:31:37Z
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

### 2026-09-03T22:31:36Z — Christopher Vachon (user)

Landed in f9904f1, gate green. A document has no type tag and MySQL still distinguishes DECIMAL from DOUBLE inside one, which cannot be recovered from the text (2.50 renders the same either way). Solved by storing a decimal with an explicit zero exponent -- nothing else in a document can wear it, since a double never renders with exponent 0 and source text is re-spelled on the way in. The mark is stripped everywhere a client can see, and FORMAT_VERSION went 10 -> 11 for the meaning change.

Deliberately composes with the in-flight decimal-scale work (#0118/#0122/#0123): the mark rides on whatever decimal text from_value is handed, so that change alters the text being marked rather than the marking.

17 mutations. One passed at first -- as_int's JSON arm was reached by nothing -- so LEFT/RIGHT/SUBSTRING/LPAD/ROUND over a JSON argument were measured on 8.3 and added until it bit.
