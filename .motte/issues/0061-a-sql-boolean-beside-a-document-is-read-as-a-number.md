---
id: 61
title: A SQL boolean beside a document is read as a number
state: Done
parent: 9
labels: [compat]
created: 2026-08-21T12:40:09Z
updated: 2026-08-25T05:12:33Z
---

## Description

Split out of #0045. MySQL reads the SQL keyword TRUE as the JSON value `true` and the literal 1 as the JSON number 1, so they compare against different documents:

    CAST('true' AS JSON) = TRUE   mysql 1   engine 0
    CAST('true' AS JSON) = 1      mysql 0   engine 0
    CAST('1'    AS JSON) = TRUE   mysql 0   engine 0
    CAST('1'    AS JSON) = 1      mysql 1   engine 1

Only the first row differs, and the engine cannot currently do better: crates/engine/src/value.rs has no boolean Value, so TRUE and 1 both evaluate to Value::Int(1) and are indistinguishable by the time a comparison sees them. sqlparser does keep them apart — Expr::Value(Boolean) versus Number — so the information exists at the expression level and is thrown away below it.

Fixing it means either a Value::Bool variant, which the image format and the wire encoding both have to learn, or converting the operand where the expression is still in hand. The same shape as #0054, where a temporal loses its type on the way into arithmetic.

## Notes

### 2026-08-25T05:12:32Z — Christopher Vachon (user)

Done in 807c1b9. Value::Bool, with json::from_value the one place it is visible. Built at bool_value -- the funnel the comparison, logical and postfix operators already shared -- rather than at the literal, because tagging only the literal leaves CAST('true' AS JSON) = (1=1) wrong.

FORMAT_VERSION does not move: a boolean cannot be stored, since every write goes through SqlType::coerce and every arm builds a value of the column's own kind. Proved empirically -- TRUE inserted into eight column kinds and the catalog walked for a stray tag -- rather than by reading the code.

The crux measured as hoped: BOOLEAN is tinyint(1) and a row holding 1 does not match a document of true, so no column can hold one.

#0068's propagation list does not transfer, in either direction. IF, CASE and NULLIF keep an ENUM's ordinal and drop a boolean; a scalar subquery drops the ordinal and keeps the boolean; a derived table keeps the ordinal and drops the boolean. Only COALESCE, IFNULL, GREATEST, LEAST, the aggregates and set operations agree. Measuring the list rather than copying it was the work.

'Otherwise it behaves as Int(0|1)' was not free: a Bool missed the integer paths in arithmetic, unary minus, SUM and ABS, each answering a float under a LONGLONG code -- invisible in a SELECT, visible in a document and on the binary protocol.

Declined one review finding on evidence: MySQL refuses INSERT INTO t(j) VALUES (TRUE) with 3140, and refuses VALUES (1) in the same words, so there is no answer to honour and honouring the tag would invent one.

Metadata fixed alongside, each because this change made half of it right: <=> was VAR_STRING and nullable, REGEXP_LIKE VAR_STRING, VALUES ROW(1) nullable where MySQL says NOT NULL. JSON_VALID stays nullable though it cannot be null, because MySQL describes it that way -- agreeing with MySQL beats being consistent with it.

STRCMP deliberately excluded: MySQL types it boolean but renders -1 as true, which needs a value-carrying boolean rather than Bool(bool).

41 mutation invocations, 40 distinct, all biting. Three needed the test strengthened rather than the mutation dropped: '1.0' = TRUE and '01' = TRUE to separate the collated and numeric readings, a UNION reached through a scalar subquery, and IF's else branch. Two are inert by construction and are asserted at their own level. Corpus 510 -> 626, baseline empty.
