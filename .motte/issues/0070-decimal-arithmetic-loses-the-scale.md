---
id: 70
title: Decimal arithmetic loses the scale
state: Done
parent: 9
labels: [executor]
created: 2026-08-21T15:22:20Z
updated: 2026-08-26T00:52:54Z
---

## Description

#0049 gave storage and the aggregates an exact decimal; arithmetic still goes through an f64 and comes back with whatever the float printed. Over a DECIMAL(10,2) a and a DECIMAL(8,3) b, both holding 1.5:

                mysql      engine
    a + b       3.000      3
    a - b       0.000      0
    a * b       2.25000    2.25
    a / b       1.000000   1
    a + 1       2.50       2.5
    a * 2       3.00       3
    a + 0.5     2.00       2
    -a          -1.50      -1.5

MySQL's rules, measured on 8.3: addition and subtraction take the larger of the two scales, multiplication adds them, division adds four to the left one, and a sign keeps it.

Every rule reads off the operands' own scales, and crates/engine/src/decimal.rs already counts a decimal in units of its last place, so the arithmetic can be exact rather than merely rounded at the end. The work is that crates/engine/src/expr.rs::arithmetic converts both sides with as_number and has nowhere to put a scale — it needs a branch for two exact operands, the way the fold in exec.rs now has one.

Not in the differential baseline: the corpus has no decimal arithmetic. Found by measuring while closing #0049.

## Notes

### 2026-08-26T00:52:54Z — Christopher Vachon (user)

Done in 7a5f988, together with #0085.

Rules measured rather than transcribed, and two of the issue's premises needed correcting. The 30-place scale clamp applies to * and / only -- a 37-place literal keeps all 37 through + - % unary minus and ABS, and comes back at 30 through * 1 or / 1. Precision is not clamped at 65 at all: two 35-digit DECIMAL(65,0) operands multiply to a 70-digit answer, printed in full, under a column still described decimal(65,0).

Confirmed: + and - take max(s1,s2), * takes min(s1+s2,30), / takes min(s1+inc,30), % takes max(s1,s2), DIV is BIGINT, unary minus and ABS keep the scale. Division computes to ceil9(s1+s2+inc) places by truncating and only then rounds half away from zero, which is why 1.00001/3 is 0.333336666 rather than ...667, and why 7/2 is 3 at increment 0.

That forced the arithmetic onto digit strings: an i128 holds 38 digits where MySQL's DECIMAL holds 65, so an exact answer cannot live in one.

Also measured: div_precision_increment is session-settable and affects / only, not DIV or MOD; a zero divisor is NULL for / % and DIV in a SELECT under every sql_mode, with 1365 belonging to statements that store; a decimal literal is exact and keeps its written places while an exponent is DOUBLE; exactness survives against INT, BIGINT UNSIGNED, another DECIMAL and a temporal, and becomes DOUBLE against FLOAT, DOUBLE or a string.

Review caught the change contradicting itself twice, both fixed. A quotient was announced DOUBLE whenever an operand could not be typed, because arithmetic_type's DOUBLE also means 'unknown', so (SELECT 1)/2 was reconciled back through an f64 and answered 0.5 where CONCAT of the same expression said 0.5000. And comparison still went through an f64 while storage and arithmetic no longer did, so over two 38-digit columns a - b was 1 while a = b was also true -- self-consistent before this commit and not after it. Fixing comparison changed no existing corpus or test answer.

The agent's own review caught two more: DIV lost the UNSIGNED tag once an operand was a decimal, and a decimal past 38 digits keyed by raw text in collation_key so one number formed two groups.

31 mutations, all bite. Filed rather than half-fixed: #0111 numeric columns described with no precision or scale, #0112 NULL literal arithmetic is DOUBLE, #0113 div_precision_increment is not a system variable, #0114 -dt3 drops its places, #0115 GREATEST/LEAST do not coerce, #0116 SIGN unsupported, #0117 unary plus not peeled from a name, #0118 a quotient's value carries more places than its column declares, #0119 a >65-digit decimal saturates through a temp table, #0120 CAST to an integer truncates where MySQL rounds.

Corpus 1266 -> 1386, baseline empty.
