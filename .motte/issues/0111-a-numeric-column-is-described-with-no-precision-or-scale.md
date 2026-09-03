---
id: 111
title: A numeric column is described with no precision or scale
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T23:12:44Z
updated: 2026-09-03T14:03:11Z
---

## Description

Every numeric result column leaves this engine with `column_length` 20 and `decimals` 0, from the defaults in `ColumnDef::new`. MySQL reports the computed pair, and reports the BINARY flag on a computed decimal besides. Measured on 8.3 over a DECIMAL(10,2) `a` and a DECIMAL(8,3) `b`:

                    mysql                        engine
    a               len 12  dec 2  NUM           len 20  dec 0  NUM
    b               len 10  dec 3  NUM           len 20  dec 0  NUM
    a + b           len 14  dec 3  BINARY NUM    len 20  dec 0  NUM
    a * b           len 20  dec 5  BINARY NUM    len 20  dec 0  NUM
    a / b           len 19  dec 6  BINARY NUM    len 20  dec 0  NUM
    a MOD b         len 12  dec 3  BINARY NUM    len 20  dec 0  NUM
    SUM(a)          len 34  dec 2  BINARY NUM    len 20  dec 0  NUM
    AVG(a)          len 16  dec 6  BINARY NUM    len 20  dec 0  NUM

The precisions are MySQL's own rules, all measured: `+`/`-` take max(int digits) + max(scale) + 1, `*` adds the two precisions, `/` is p1 + s2 + div_precision_increment, and `%` takes the wider of the two *lengths* and converts that back to a precision. The length is precision + 1 for the point where the scale is nonzero + 1 for the sign.

A NEWDECIMAL goes on the wire as a length-encoded string in both protocols, so the digits are right today whatever this says — but a client that reads the metadata is told a decimal column has no places, which is the same claim the value contradicts.

Not caught by the differential harness: `crates/differential/src/compare.rs` compares the name, the type code, nullability and the UNSIGNED flag, and not the length or the decimals. See #0087, which names the same blind spot for ENUM and SET. Widening the harness is the first half of this.

Found while measuring for #0070.

## Notes

### 2026-08-28T17:00:29Z — Christopher Vachon (user)

Partly provable after #0126 (a2e3d47): a DECIMAL column's declared length is now compared and correct -- 12 for DECIMAL(10,2), 11 for (10,0), 67 for (65,30), 9 for (8,3) UNSIGNED, the rule being M + a point if there is a scale + a sign if signed.

Still not compared, and still this issue's subject: the decimals field itself (1148 divergences over 260 statements when it was measured), and the width of a computed column, which MySQL takes from Item::max_length and the engine does not model at all (2784 over 560).

Both are named with those counts in compare.rs and in crates/differential/README.md. Turning either on is the first step of whoever takes this -- the counts above are what to expect.

### 2026-09-03T14:03:11Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. Over DECIMAL(10,2) and DECIMAL(7,3): 8.3 reports len 12 dec 2, len 10 dec 3, and len 14 dec 3 BINARY NUM for a+b; the engine reports len 20 dec 0 NUM for all three.
