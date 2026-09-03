---
id: 120
title: CAST to an integer truncates where MySQL rounds
state: Done
parent: 9
labels: [executor]
created: 2026-08-26T00:18:26Z
updated: 2026-09-03T05:11:02Z
---

## Description

Found by review during #0070, and pre-existing -- the same answer at HEAD for both literals and DECIMAL columns.

    SELECT CAST(1.5 AS SIGNED)    mysql 2   engine 1
    SELECT CAST(2.5 AS SIGNED)    mysql 3   engine 2

MySQL rounds half away from zero; this engine truncates toward zero.

Worth doing now because #0070 made it inconsistent with its neighbours: decimal::units already implements half-away-from-zero exactly, and #0079 measured and encoded the same rule for a bitwise operand, so the cast is the one numeric reader still truncating. Check CAST(... AS UNSIGNED) and the negative cases in the same pass, and whether an approximate operand (a DOUBLE) rounds the same way an exact one does.

## Notes

### 2026-09-03T05:11:02Z — Christopher Vachon (user)

Done in 6d6ade9 (landed with #0102).

Three readings, picked by the operand's type -- not one rule, which is the #0130 split again. An exact decimal (literal, column, arithmetic, CAST AS DECIMAL) rounds HALF AWAY FROM ZERO: 1.5->2, 2.5->3, -2.5->-3. An approximate one (DOUBLE, FLOAT) rounds HALF TO EVEN: 1.5e0->2, 2.5e0->2, 4.5e0->4, 0.5e0->0. A string TRUNCATES to its leading integer: '1.5'->1, '1.9'->1, '1e30'->1, 'inf'->0.

Truncating coincided with half-to-even wherever the kept digit was already even, which is why 2.5e0 looked right and 1.5e0 did not -- the obvious test would have confirmed the wrong rule.

Ranges differ per reading too. An exact decimal saturates into the TARGET's range (i64::MAX / u64::MAX, and the negative pair). An approximate one goes through a SIGNED longlong whichever target is named, so CAST(1e19 AS UNSIGNED) and CAST(1e30 AS UNSIGNED) are both 9223372036854775807. A string saturates at the UNSIGNED boundary and the target then reinterprets, so CAST('99999999999999999999' AS SIGNED) is -1 where the same digits as a decimal literal are i64::MAX. An integer literal above i64::MAX is unsigned to MySQL, so CAST(10000000000000000000 AS SIGNED) is -8446744073709551616.

Nothing errors anywhere -- warning 1292 only -- and sql_mode changes none of it (tested '', STRICT_ALL_TABLES, TRADITIONAL).

Review caught the first cut reading a string through as_exact_int's f64 fallback, so CAST('1e30' AS SIGNED) answered -1 against MySQL's 1 and 'inf' answered -1 against 0. A JSON number keeps the number reading, measured separately.

decimal::whole now owns 'a decimal as an integer' in one place, and is_plain_decimal is gone -- it existed only to disambiguate units' None, which whole now does. #0130's hex coverage was re-verified through the refactor.

15 mutations, all bite. Left untouched and unclaimed by the corpus: CONVERT(x, SIGNED) is 1235 here (a refusal, not a wrong answer), CAST(X'0F' AS SIGNED) is #0141, and a JSON string spelling a number is #0121 territory.
