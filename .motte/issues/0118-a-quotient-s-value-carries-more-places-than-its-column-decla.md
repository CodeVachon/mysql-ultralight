---
id: 118
title: A quotient's value carries more places than its column declares
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-25T23:29:12Z
updated: 2026-09-03T14:20:27Z
---

## Description

MySQL's decimal division produces a value with more fractional digits than the column it is described with, and both halves are observable. Measured on 8.3:

    SELECT 1/3                  0.3333          the column is decimal(5,4)
    SELECT JSON_ARRAY(1/3)      [0.333333333]   the value has nine places
    SELECT 1/3 = 0.3333         0               so the comparison is false
    SELECT (1/3)*3              1.0000          0.333333333 * 3, shown at four

The extra digits come from MySQL computing a quotient to a whole number of nine-digit words and truncating there; the declared `decimals` — the left operand's scale plus `div_precision_increment` — governs only the rendering. The width is not a simple function of the two scales: measured, `1/3` and `1.5/1` and `1/1.5` carry 9 places, `1.5/1.5` and `1.000000/1` and `1/1.00000000000` carry 18, and `1.0000000000000000000000000/7` carries 36.

Where it shows up:

    SELECT SUM(v/3) FROM P          -- 32768 rows of DECIMAL(10,2), v in 1.50..8.50
        mysql   54613.333324        -- totals the nine-place values
        engine  54613.331968        -- totals the four-place ones
    SELECT JSON_ARRAY(1/3)
        mysql   [0.333333333]
        engine  [0.3333]
    SELECT 1/3 = 0.3333
        mysql   0
        engine  1

#0070 and #0085 rounded the quotient to the *displayed* scale in the value itself, which is what makes every printed quotient right and is all the engine can do today: `Value::Decimal` carries its scale in its own text and there is no declared scale beside it to round against — see #0111, which is that a column is described with no precision or scale at all. Closing this properly means the value and the column carrying different scales, and something at the edge rounding one to the other. `ResultSet::reconcile` is where that would go.

Left out of the differential corpus for the same reason: `crates/differential/src/cases.rs` covers division through what it prints, and the group says so.

Found while cross-checking a 32768-row aggregate against 8.3 after #0070.

## Notes

### 2026-09-03T14:01:18Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed exactly, including the width pattern. JSON_ARRAY(1/3) mysql [0.333333333] engine [0.3333]; 1/3=0.3333 mysql 0 engine 1; (1/3)*3 mysql 1.0000 engine 0.9999. JSON_ARRAY(1.5/1, 1/1.5, 1.5/1.5, 1.000000/1) confirms 9/9/18/18 places on 8.3 against 5/4/5/10 here.
