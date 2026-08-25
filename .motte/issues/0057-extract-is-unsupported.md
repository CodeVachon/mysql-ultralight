---
id: 57
title: EXTRACT is unsupported
state: Done
parent: 9
labels: [executor]
created: 2026-08-20T20:16:48Z
updated: 2026-08-25T13:45:17Z
---

## Description

SELECT EXTRACT(YEAR FROM NOW()) fails with 1235 "does not yet support expression `EXTRACT(YEAR FROM NOW())`". MySQL answers 2026.

sqlparser gives EXTRACT its own Expr::Extract variant rather than a function call, which is why it never reached the evaluator. #0040 already types the variant as LONGLONG — correct, and unreachable until this lands. The unit list is the work: MySQL takes YEAR, QUARTER, MONTH, WEEK, DAY, HOUR, MINUTE, SECOND, MICROSECOND and the compound forms like YEAR_MONTH and DAY_SECOND, most of which crates/engine/src/temporal.rs can already answer.

Found while measuring column names for #0043. A clear error today, not a wrong answer.

## Notes

### 2026-08-25T13:45:17Z — Christopher Vachon (user)

Done in f75a879. Answers the nine units sqlparser can hand it.

The unit decides how the argument is read, which the issue did not anticipate: MySQL converts to a DATETIME for the calendar units and to a TIME for the clock ones. So the sign rides on every clock field and none of the calendar ones -- over -06:07:08, EXTRACT(HOUR ...) is -6 where HOUR() on the same value is 6 -- and over a TIME column EXTRACT(DAY ...) is a day of today's month, with a negative TIME landing on yesterday.

b95cca0's tag is load-bearing: EXTRACT(SECOND FROM d) over a DATE column is 0 while EXTRACT(SECOND FROM '2024-01-05') is 24, because MySQL reads that string as the time 00:20:24.

WEEK honours default_week_format, all eight modes transcribed from calc_week and checked against 8.3 over eight dates each. The variable came with it: clamped 0..7 with warning 1292 where MySQL clamps, and 1232 for a non-integer -- which includes a string spelling a number, so '3' and 3.7 raise where TRUE gives 1 and 1 + 1 gives 2.

The unit set EXTRACT accepts is exactly the set INTERVAL accepts, verified across all 20 plus 9 non-MySQL names.

What cannot be read exactly refuses rather than guessing. Review found a single whitespace character crossing that boundary: the guard measured length on the trimmed text and looked for a space in the untrimmed one, so a padded T-separated shape answered 8 where its unpadded twin refused and MySQL says 24. MySQL's own trimming is not symmetric -- a leading space does not count toward the twelve, a trailing one does -- which is now recorded and is what makes the length half testable.

Also fixed: 3b6a342's as_int deliberately wraps a u64 above i64::MAX negative, so the week-format clamp went down to 0 instead of up to 7. Now reads as_exact_int. The same pattern survives in functions.rs for SPACE, LEFT, RIGHT, LPAD and RPAD -- filed as #0097.

Filed rather than half-done: #0094 (the eleven compound units do not parse -- sqlparser has no keyword and allow_extract_custom is false; packing rules measured and recorded there), #0095 (SET validation missing for every other variable), #0096 (SHOW VARIABLES ignores its LIKE pattern, which is why this could not get corpus coverage).

25 mutations, all bite. Three needed the test strengthened rather than the guard weakened. Corpus 968 -> 1044, baseline empty.
