---
id: 223
title: ROUND over a double rounds half away from zero, not half to even
state: Todo
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T04:08:08Z
updated: 2026-09-04T04:08:08Z
---

## Description

Found while implementing #0212/#0215 and flagged by that agent as the highest-value divergence it saw. Measured on 8.3:

    SELECT ROUND(0.5e0), ROUND(1.5e0), ROUND(2.5e0), ROUND(-2.5e0), ROUND(4.5e0)
    mysql   0  2  2  -2  4     (half to EVEN)
    engine  1  2  3  -3   5    (half AWAY from zero)

Exact decimals agree -- it is only the approximate path.

Cause: functions.rs's "round" arm calls .round() where round_ties_even() is needed for an approximate operand. This is the SAME SPLIT #0195 established for the write path (a DECIMAL rounds half away from zero, a DOUBLE half to even) and that #0212 modelled for FORMAT -- so the rule is already written down twice in this codebase and ROUND is the remaining reader that does not follow it.

Priority 2: the target application calls ROUND 26 times, and a wrong .5 on a money column is a wrong answer rather than a metadata difference.

Note the same agent measured that a STRING argument takes the approximate path, because MySQL picks by declared result type and only INT and DECIMAL are exact -- so ROUND('2.5') should be 2, not 3.
