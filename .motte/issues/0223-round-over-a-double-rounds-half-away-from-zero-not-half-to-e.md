---
id: 223
title: ROUND over a double rounds half away from zero, not half to even
state: Done
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T04:08:08Z
updated: 2026-09-04T09:46:49Z
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

## Notes

### 2026-09-04T09:46:49Z — Christopher Vachon (user)

Landed with the stack work, gate green. ROUND('2.5') IS 2 -- confirmed on 8.3 and now here. MySQL picks by the argument's DECLARED RESULT TYPE, so only INT/BIGINT/DECIMAL and TRUE are exact (half away from zero); a string, DOUBLE, FLOAT, JSON scalar, binary string and temporal all go through a double and round half to EVEN.

The fix was larger than swapping .round() for .round_ties_even(), because measuring showed the EXACT path was also wrong: ROUND(1.005, 2) was 1 rather than 1.01, a bigint round lost precision through an f64, and ROUND(<DECIMAL(10,3)>, 2) dropped the declared scale. The values had been diverging from the engine's own declared types.

TRUNCATE's rounding is unchanged and was measured as the control.

Review's finding 3 was worse than reported (x * 10^places then / 10^places is not x / 10^|places| then * -- ~50% of random doubles wrong at scale -5) and finding 4 wrapped at BOTH ends of i64, not just MIN. A third fell out of fixing them: 10f64.powi(300) is six ulps above the literal 1e300.
