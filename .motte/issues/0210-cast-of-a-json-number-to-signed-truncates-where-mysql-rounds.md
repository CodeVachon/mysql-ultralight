---
id: 210
title: CAST of a JSON number to SIGNED truncates where MySQL rounds
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T22:31:37Z
updated: 2026-09-03T22:31:37Z
---

## Description

Found while implementing #0084/#0121.

    CAST(json_number AS SIGNED)   8.3 rounds half to even, the engine truncates
    2.5 -> 2 / 2 (agrees)   3.5 -> 4 / 3   2.9 -> 3 / 2   -2.5 -> -2 / -2

Same family as #0195, which taught the write path that a DECIMAL rounds half away from zero and a DOUBLE half to even. This is the CAST path over a value that came out of a document, and it still truncates.

Also measured at the same time: JSON_TYPE answers UNSIGNED INTEGER on 8.3 for CAST(1 AS UNSIGNED), '9223372036854775807' and '18446744073709551615' where the engine says INTEGER; and DATE/TIME/DATETIME for temporal values where the engine says STRING. Renderings agree in every case -- only the type name differs.
