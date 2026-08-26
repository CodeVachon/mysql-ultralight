---
id: 119
title: A decimal wider than 65 digits saturates through a temp table
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T23:42:34Z
updated: 2026-08-25T23:42:34Z
---

## Description

MySQL computes a product past DECIMAL's 65-digit maximum and prints every digit — until the value has to be *materialised*, at which point it saturates to 65 nines. Measured on 8.3 over a DECIMAL(65,0) holding 99999999999999999999999999999999999:

    SELECT big*big FROM W                    9999999999999999999999999999999999800000000000000000000000000000000001
    SELECT big*big FROM W ORDER BY 1         the same 70 digits
    SELECT v FROM (SELECT big*big AS v ...)  the same 70 digits
    SELECT big*big FROM W GROUP BY big*big   99999999999999999999999999999999999999999999999999999999999999999
    SELECT DISTINCT big*big FROM W           the same 65 nines

So it is the temp table, not the operator: the expression's declared type is `decimal(65,0)` in all five, and only the two that go through one clamp the value to what that type can hold. No warning is raised for it.

This engine answers the 70 digits everywhere, which matches three of the five.

Same family as #0110 — a value materialised differently from how it is computed — and it needs the same thing: somewhere that knows a temp column's declared type and rounds or saturates to it on the way in.

Left out of the differential corpus's `decimal arithmetic` group, which keeps the 70-digit product to the four shapes that agree. Found while widening the collation key for #0070.
