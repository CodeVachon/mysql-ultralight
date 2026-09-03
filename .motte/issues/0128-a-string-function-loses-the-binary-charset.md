---
id: 128
title: A string function loses the binary charset
state: Todo
parent: 9
labels: [compat]
created: 2026-08-26T11:18:03Z
updated: 2026-09-03T14:04:20Z
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

## Notes

### 2026-08-31T21:18:30Z — Christopher Vachon (user)

#0130 (89b225a) makes this measurable and narrows it considerably.

HEX now exists, so the byte-level half can be asked directly. Measured over b = 'éabc' in a VARBINARY column:

    HEX(UPPER(b))       mysql C3A9616263   engine C389414243

That is the exact byte where the binary charset is lost, and it is now expressible as a corpus case the moment this is fixed.

The other three measurements in this issue -- CONCAT(b,''), SUBSTRING(b,1), TRIM(b) -- turn out to AGREE through HEX. The bytes survive those functions; only the charset label is lost. So the value half of this issue is confined to case-folding (UPPER, and presumably LOWER), and the rest is a metadata question the harness's column-charset comparison already makes.

Note #0140 was filed separately for a different UPPER bug found the same way (it expands ß to SS where MySQL does not), and #0143 for the same root cause one level up -- binariness lost through an aggregate. Those three plus this are one mechanism: a charset is a property of the expression in MySQL and only of the Value here.

### 2026-09-03T14:04:20Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed exactly, all four: SOUNDEX of CONCAT/SUBSTRING/TRIM over a VARBINARY holding 'éabc' is A120 on 8.3 and é120 here; through UPPER it is A120 there and É120 here.
