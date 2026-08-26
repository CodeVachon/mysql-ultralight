---
id: 75
title: FLOAT(M,D) prints a fixed number of decimals
state: In Progress
parent: 9
labels: [compat]
created: 2026-08-21T19:37:25Z
updated: 2026-08-26T11:18:10Z
---

## Description

FLOAT(10,4) holding 1.234567890123, 0.1, 1.5
    mysql   1.2346   0.1000   1.5000
    engine  1.23457  0.1      1.5

#0048 gave a plain FLOAT its four bytes and MySQL's six significant digits. The two-argument form displays differently again: D decimal places, always, the way a DECIMAL does — while still being four bytes underneath, and still reported as MYSQL_TYPE_FLOAT with the decimals metadata set to D.

Value::Float32 carries no scale, so the renderer cannot know. The choices are to give the variant one, or to notice that a FLOAT(M,D) is displayed exactly as a DECIMAL is and coerce it that way — which would need checking against the binary protocol, because a Decimal maps to a length-encoded string and the column's type code would still be Float.

MySQL has deprecated the form: 8.0.17 warns that "specifying the number of digits for FLOAT is deprecated and will be removed in a future release". Nothing in the target schema uses it.
