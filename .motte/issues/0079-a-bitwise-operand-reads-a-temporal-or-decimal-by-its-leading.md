---
id: 79
title: A bitwise operand reads a temporal or decimal by its leading digits
state: Done
parent: 9
labels: [executor]
created: 2026-08-24T17:36:11Z
updated: 2026-08-24T20:58:12Z
---

## Description

Regression introduced by #0052 in 846c925, found by review during #0053.

bit_operand runs integer_prefix over a Value::Text, so any value the engine stores as text is read by its leading digits rather than converted the way MySQL converts it:

    CAST('2026-01-02' AS DATE) | 0            mysql 20260102   engine 2026
    CAST('3.7' AS DECIMAL(10,1)) | 0          mysql 4          engine 3

The decimal case is the plainer bug: #0052 measured and encoded that a number operand rounds away from zero, and a decimal that reached the operator as text never reaches that rule. The temporal case is the same shape as #0054 -- the value has lost its declared type before the operator sees it -- and will likely fall out of whatever #0054 does.

A wrong answer rather than a refusal, which the project rule forbids. Until the type reaches the operator, the honest behaviour is 1235.

## Notes

### 2026-08-24T20:58:12Z — Christopher Vachon (user)

Done in 1ee4c9e. Two bugs wore one coat.

The decimal half is answered: CAST(x AS DECIMAL(p,s)) yields a tagged value at the declared scale, and the operand is counted through decimal::units, whose rounding already is MySQL's half-away-from-zero rule. An f64 loses the tie past fifteen digits -- DECIMAL(20,1) 4503599627370496.5 answered ...496 where MySQL says ...497. Tagging the cast corrected it on its own too: CAST('-2.5' AS DECIMAL) was -2.5 against MySQL's -3, CAST('3.75' AS DECIMAL(10,1)) was 3.75 against 3.8. A rescale that overflows an i128 is now done on the digit string, so a DECIMAL(40,30) reaches its declared scale rather than keeping what it was written with; MySQL caps scale at 30, so DECIMAL(65,40) is 1425 and not reachable.

The temporal half refuses, and is #0054's to answer: the packed YYYYMMDD integer depends on the operand's declared type, and ColumnRef carries a table and a name.

Where the refusal is drawn matters more than that it exists. Where the statement declares the result a string -- a literal, CAST(x AS CHAR), CONCAT, LEFT, and every function the metadata already types VARCHAR -- the operand reads as a string, because that type is as declared as a column's. Review caught the first cut refusing all of those, which had answered correctly before. Measured rather than reasoned: COALESCE(d,'0') and IFNULL(d,'0') are strings to MySQL, while IF(1,d,'0'), GREATEST(d,'0') and CASE keep the temporal -- so the first two answer and the rest refuse.

Still refused: a temporal column, a cast to one, ~d, and a VARCHAR holding date-shaped text. The last is indistinguishable from a DATE column, which is the defect itself.

Review also caught a HIGH regression: stacked unary signs concatenated into '--2.5', so - -2.5 & 3 answered 0 where it had correctly answered 3. number_token now counts signs rather than prefixing them, and sees through Expr::Nested.

25 mutations, 24 bite. The one that does not -- the guard covering Value::Json -- is unreachable by construction, because a JSON string scalar keeps its quotes and "2026-01-02" is not date-shaped. Said so in the comment rather than implying it is tested. Corpus 312 -> 342, baseline empty.
