---
id: 54
title: Temporal values read as their leading digits in arithmetic
state: Done
parent: 9
labels: [executor]
created: 2026-08-20T19:38:59Z
updated: 2026-08-25T00:52:41Z
---

## Description

Found by review during #0042. MySQL converts a temporal to its packed YYYYMMDD[HHMMSS] integer in an arithmetic context; this engine reads the leading digits of the text.

    SELECT d + 0   -- DATE 2024-01-05           engine 2024      mysql 20240105
    SELECT dt + 0  -- DATETIME 2024-01-05 06:07:08  engine 2024   mysql 20240105060708
    SELECT tm + 0  -- TIME 06:07:08              engine 6         mysql 60708

Before #0042 these were NULL, because arithmetic answered NULL for anything it could not parse as a whole number. #0042 made a non-numeric operand read as its leading numeric part, which is right for strings and turned this from visibly broken into quietly wrong.

The engine cannot currently tell the two apart. A temporal is stored as Value::Text, and MySQL's answer depends on the operand's *type*, not its text: a DATE column gives 20240105 where the string literal '2024-01-05' gives 2024 — both verified on 8.3. Sniffing the text shape in Value::as_number would fix the column and break the literal, so it is a coin flip rather than a fix.

Doing this properly means the arithmetic path knowing the operand's declared type, either by tagging temporals in Value the way Value::Json already tags documents, or by routing the conversion through the expression's type rather than its value.

## Notes

### 2026-08-25T00:52:41Z — Christopher Vachon (user)

Done in b95cca0. Value::Temporal, chosen over routing through the expression's type -- and the deciding evidence is a measurement, not an argument: MySQL types DATE_ADD('2024-01-05', INTERVAL 1 DAY) as STRING (checked with --column-type-info) and still reads it as 20240106, so a type-driven conversion answers 2024. It would also need inference through subqueries, unions, derived tables and MAX() to still get that case wrong. The tag rides on the same value MySQL's answer rides on.

FORMAT_VERSION 5 -> 6. Tag 8 appended, nothing reordered. A v5 image is refused rather than reloading every temporal as text, which would reintroduce this bug on a --state boot.

The tag alone was not enough. The engine stored a temporal as the characters it was given, so a DATETIME holding '2024-01-05' packed to 20240105 against MySQL's 20240105000000, and a TIME(0) holding '06:07:08.567' was typed LONGLONG while its value kept a fraction -- text sent 60708.567, binary parsed it as an i64, failed, and sent 0. temporal::normalise now writes a temporal in the shape and precision its type declares, so the type and the value cannot disagree. Precision also had to learn to travel: through DATE_ADD/ADDDATE/TIMESTAMPADD, COALESCE/IFNULL/IF/GREATEST/LEAST/NULLIF, MIN/MAX and CASE, read off the same argument computed_column_type types by.

Why the corpus missed the protocol split: every fraction in it lived in a DATETIME(3) or TIME(3), where the precision is declared and type and value agree by construction. There was no temporal carrying a fraction its type does not declare -- the one pairing that splits the protocols. Both protocols were always being run; no case produced the shape.

Measured and encoded besides: COALESCE/IFNULL aggregate types where IF/GREATEST/CASE do not, and the two genuinely disagree; -d is DOUBLE while +d is the identity; YEAR is UNSIGNED and an integer type; NOW(3)/CURTIME(3) are NOT NULL and NOW(NULL) is a syntax error, so the clock functions needed their own not-null rule rather than the argument-count one measured on UNIX_TIMESTAMP.

Every temporal shape 1ee4c9e refused now answers, including the VARCHAR holding date-shaped text that commit called the defect itself. Only ~d still refuses, for #0077's reason.

38 mutations. Two stopped biting once normalisation made their targets unreachable from SQL, and both were re-asserted at their own level rather than deleted or contrived -- the reconcile backstop against a Decimal under an integer code, and temporal_number's group widening. Corpus 342 -> 418, baseline empty.
