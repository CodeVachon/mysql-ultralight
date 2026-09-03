---
id: 139
title: A binary string compares against text through the collation
state: Done
parent: 9
labels: [executor]
created: 2026-08-31T20:28:05Z
updated: 2026-09-03T03:36:44Z
---

## Description

Found while implementing #0130, and a live wrong answer.

    SELECT CAST('AB' AS BINARY) = 'ab'   mysql 0   engine 1
    SELECT X'4142' = 'ab'                mysql 0   engine 1
    a VARBINARY column against a string literal -- same

crates/engine/src/value.rs's Value::compare takes the byte-exact branch only when BOTH operands are Value::Bytes; a Bytes/Text pair falls through to collate(as_text, as_text), which is case-insensitive. MySQL makes the comparison binary if either side is.

This is why UNHEX('4142') = 'ab' could not become a differential case in #0130 -- it diverges today.

Adjacent to #0128 (a string function loses the binary charset) but distinct: there the binariness is lost on the way out of a function, here it is present on one side and ignored by the comparison. Whoever takes one should read the other.

## Notes

### 2026-09-03T03:36:44Z — Christopher Vachon (user)

Done in 21e955c, in an isolated worktree, after four review passes -- three of which found real bugs.

The rule is the CHARACTER SET, not the coercibility, and a hex literal settles it: X'4142' and a string literal tie at coercibility 4, and the hex literal still wins, because binary beats every other character set. So either operand being Bytes decides the comparison; every pair without one is untouched.

Byte-exact now: =, <, >, <>, <=>, IN, BETWEEN, CASE, NULLIF, GREATEST, LEAST over a mixed pair, and a VARBINARY column against a string. Still collated: every pair with no Bytes operand. Still numeric: Bytes beside a numeric tag. LIKE never called compare and is unchanged.

An explicit COLLATE utf8mb4_bin is NOT the same as binary -- it compares case-sensitively like binary but PADS a trailing space, where binary does not. This does not make it answerable; still 1235, still #0149.

Trailing space measured and deliberately not encoded: binary never pads, and text depends on the collation -- 'a' = 'a ' is 0 under utf8mb4_0900_ai_ci and 1 under the PAD SPACE general_ci the harness client negotiates. A corpus case and a unit assertion that would have baked in one of the two were pulled. That is #0129.

Review found three regressions the first cut introduced, each worse than what it fixed: CAST(... AS BINARY) went through the text so a value did not equal its own cast; as_exact_int saturated at i64::MAX so a BIGINT UNSIGNED given 18446744073709551615 silently STORED 9223372036854775807 instead of raising 1366; and the grouping key's escape marker collided with the NULL sentinel in exec.rs, so a NULL row folded into a value's group and the group reported itself as holding those bytes while IS NULL disagreed.

Two mutations passed and both were the test's fault: a UNIQUE over one VARBINARY is a Bytes/Bytes pair and never exercises the mixed comparison, and the escape marker was uncovered until UNHEX('80') = '80' was added.

18 mutation invocations. Corpus 2660 -> 2736, baseline empty, both gates verified independently on the merged tree.

Noted for #0149: its refusal is itself an instance of the zero-rows pattern -- it fires with rows and with LIMIT 0, and silently answers over an empty table and in an untaken CASE branch.

Filed: #0159 (a non-numeric string compared to a number: 'abc' = 0 is 1 on 8.3 and 0 here, and 'nan' = 0 returns NULL, a three-valued-logic violation).
