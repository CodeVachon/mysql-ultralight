---
id: 114
title: Unary minus over a fractional temporal drops its declared places
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T23:13:29Z
updated: 2026-08-25T23:13:29Z
---

## Description

`-dt3` over a DATETIME(3) is a DOUBLE in MySQL and still prints all three of the column's declared places. Measured on 8.3, holding 2024-01-05 06:07:08.500:

    SELECT -dt3     mysql -20240105060708.500     engine -20240105060708.5
    SELECT -tm3     mysql -20260102030405.250     engine -20260102030405.25

The type code agrees — both say DOUBLE — and so does the number. What differs is the spelling: MySQL reports `decimals` 3 on that DOUBLE and formats to it, the way it does for a FLOAT(M,D). The engine formats an f64 by its shortest round-trip.

`dt3 + 0` is a NEWDECIMAL on both and already right; it is only the unary minus, which MySQL computes in a double rather than in the packed decimal.

Same mechanism as #0075 — a fixed number of places on an approximate column — so the two probably close together.

Left out of the differential corpus's `decimal arithmetic` group for this reason, with a comment saying so. Found while measuring for #0070.
