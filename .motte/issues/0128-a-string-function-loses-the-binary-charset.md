---
id: 128
title: A string function loses the binary charset
state: Todo
parent: 9
labels: [compat]
created: 2026-08-26T11:18:03Z
updated: 2026-08-26T11:18:03Z
---

## Description

Found while implementing #0074, which fixed SOUNDEX reading a binary string and left this.

Over a VARBINARY column holding 'éabc', MySQL keeps the binary charset through a string function and answers A120 in every case. This engine answers:

    SOUNDEX(CONCAT(b,''))     mysql A120   engine é120
    SOUNDEX(SUBSTRING(b,1))   mysql A120   engine é120
    SOUNDEX(TRIM(b))          mysql A120   engine é120
    SOUNDEX(UPPER(b))         mysql A120   engine É120

UPPER is a third answer rather than a fourth instance of the same one: by then the bytes have become text, so the accent is case-folded too.

The cause is that a charset is a property of the expression in MySQL and only of the Value here -- crates/engine/src/functions.rs returns Value::Text from every string function whatever it was given, so binariness survives exactly one call.

A class rather than a corner, and it reaches further than SOUNDEX: anything whose answer depends on whether its input is bytes or characters inherits it.

No differential case can exist while the baseline must stay empty -- every statement that exposes it diverges today.

Related: #0126 (a column's declared length and the BINARY flag are never sent), and the per-column charset gap below.
