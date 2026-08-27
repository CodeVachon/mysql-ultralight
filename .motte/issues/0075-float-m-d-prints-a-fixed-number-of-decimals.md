---
id: 75
title: FLOAT(M,D) prints a fixed number of decimals
state: Done
parent: 9
labels: [compat]
created: 2026-08-21T19:37:25Z
updated: 2026-08-26T16:18:32Z
---

## Description

FLOAT(10,4) holding 1.234567890123, 0.1, 1.5
    mysql   1.2346   0.1000   1.5000
    engine  1.23457  0.1      1.5

#0048 gave a plain FLOAT its four bytes and MySQL's six significant digits. The two-argument form displays differently again: D decimal places, always, the way a DECIMAL does — while still being four bytes underneath, and still reported as MYSQL_TYPE_FLOAT with the decimals metadata set to D.

Value::Float32 carries no scale, so the renderer cannot know. The choices are to give the variant one, or to notice that a FLOAT(M,D) is displayed exactly as a DECIMAL is and coerce it that way — which would need checking against the binary protocol, because a Decimal maps to a length-encoded string and the column's type code would still be Float.

MySQL has deprecated the form: 8.0.17 warns that "specifying the number of digits for FLOAT is deprecated and will be removed in a future release". Nothing in the target schema uses it.

## Notes

### 2026-08-26T16:18:31Z — Christopher Vachon (user)

Done in eb9d89a. The premise was half wrong: FLOAT(M,D) is not a display rule bolted onto a plain FLOAT. MySQL rounds the stored value to D places on the way in, ties to even, before narrowing to four bytes -- visible in JSON_ARRAY, in SUM, and in the bytes the binary protocol sends. A value bug with a rendering half.

Coercing to a Decimal, which this issue suggested, is measurably wrong: f*f is four places in MySQL where decimal multiplication gives eight, and f/2 is the double quotient at eight places where exact division differs. DOUBLE(M,D) follows the same rule. REAL(M,D) does not parse at all (1064), which two comments claimed it did.

Range is checked after rounding: 9999.994 stores, 9999.995 is 1264. Non-strict clamps with warning 1264. The deprecation warning is 1681 and is not implementable here -- the engine has no warning channel, SHOW WARNINGS is 1235.

Review found padding could not go through format!("{:.*}"), which prints the double's binary expansion: DOUBLE(20,17) holding 0.1 came back 0.10000000000000001 against MySQL's 0.10000000000000000. MySQL pads the shortest round-tripping string, which is what its own dtoa gives, so the text is built from decimal::plain and rescaled through the existing machinery. That also exposed a second storage bug -- rounded_to_scale divided back by the power of ten unconditionally, so DOUBLE(255,30) holding 1.5 stored 1.5000000000000002.

Negative zero measured as a storage rule rather than a rendering one: 8.3 answers 0.00, [0.0] in JSON and f32:0.0 on the wire, so normalising in rounded_to_scale fixes all three. -0.006 keeps its sign because its rounding never reaches zero.

A declared scale had never been validated and format made it reachable: DECIMAL(10,1000000) already wrote a million characters, and a cast could ask for four billion from one SELECT. Both paths are bounded, and their rules differ -- a cast checks precision, then M >= D, then scale, so the same declaration answers 1426 where a column answers 1425.

Set operations measured: scale is the max, not-fixed if either side is, width is max(M-D) + max(D), and the rounding precedes the dedupe so f UNION d is one row. FLOAT(10,4) with DECIMAL(10,3) is deliberately excluded -- 8.3 says double(13,4) and the 13 does not follow from the rule the approximate pairs give, so it was left rather than guessed.

Two wrong answers found next door and fixed: a view reconciled its own result, so a double(10,4) read through one lost its places while the float(10,4) beside it kept them; and CAST(f AS DECIMAL) read the six-digit display text rather than the number.

Left and flagged: a row predating an ALTER that adds a scale keeps its old bytes in MySQL (rows are rewritten only when the storage width changes), and the engine now re-renders them. Matching it needs the scale inside as_text() -- the tagged-value design ruled out above. See #0131.

15 mutations bite; one does not and says so -- powi equals every literal in the power-of-ten table on this host, so no test distinguishes them. Corpus 1528 -> 1612, baseline empty, verified by re-running the harness independently.
