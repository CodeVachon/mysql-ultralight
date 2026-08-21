---
id: 49
title: DECIMAL loses its scale through an aggregate
state: Done
parent: 9
labels: [executor, compat]
created: 2026-08-20T17:57:30Z
updated: 2026-08-21T15:43:07Z
---

## Description

Split out of #0040, which is about the type a column is *described* with. This is about the value.

MySQL keeps a DECIMAL's scale through an aggregate: SUM over a DECIMAL(10,2) of 1.50 and 2.50 is 4.00, and AVG of the same is 2.000000 - AVG adds four digits of scale to the argument's, which is its documented rule. This engine answers 4 and 2, because a DECIMAL is held as text and folded through f64, so the scale is whatever the arithmetic left behind.

Visible in the harness as row divergences on SELECT k, COUNT(*), SUM(v), MIN(s), MAX(v), AVG(v) FROM G GROUP BY k, in both protocols.

Separate from #0040 because describing the column as NEWDECIMAL - which it now is - does not make the value right, and fixing the value does not need the type work. It probably needs decimal arithmetic that tracks scale rather than going through a float, which is its own piece of work.

## Notes

### 2026-08-20T18:56:49Z — Christopher Vachon (user)

Wider than the title: a plain stored DECIMAL loses its scale too, not only one through an aggregate. INSERT 1.5 into DECIMAL(6,2) reads back 1.5 where MySQL says 1.50, and 2 reads back 2 where MySQL says 2.00. Found while fixing #0041.

### 2026-08-21T15:21:46Z — Christopher Vachon (user)

All 4 lines fixed; baseline 15 to 11. A new crates/engine/src/decimal.rs counts a decimal in units of its last place, so storage renders at the column's scale and SUM and AVG are exact rather than folded through an f64. Rounding is half away from zero, which is MySQL's rule and not what a float prints: -1.005 into a DECIMAL(10,2) is -1.01, and 2.5 into a scale of nothing is 3. Not done: general arithmetic does not track scale yet, so a+b over two decimals still goes through a float — see the new issue.

### 2026-08-21T15:22:28Z — Christopher Vachon (user)

The arithmetic follow-up is #0070.

### 2026-08-21T15:43:07Z — Christopher Vachon (user)

Review found that rescaling storage broke every comparison over a decimal: two Value::Text compare by characters, so a=2.5 matched nothing, a cross-scale join found no rows, and ORDER BY SUM put 10.00 first. The engine could not tell a decimal from a VARCHAR, and it must — MySQL compares '2.5' and '2.50' as strings and calls them different, and as decimals and calls them equal. Value::Decimal is tagged for exactly the reason Value::Json is, FORMAT_VERSION goes to 4.
