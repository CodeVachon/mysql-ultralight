---
id: 69
title: Unsigned arithmetic out of range should be an error
state: Done
parent: 9
labels: [executor]
created: 2026-08-21T14:04:47Z
updated: 2026-08-25T08:17:18Z
---

## Description

MySQL refuses rather than answering when arithmetic on an UNSIGNED operand leaves its range:

    SELECT iu - 100 FROM Q   -- iu INT UNSIGNED holding 7
    mysql   ERROR 1690 (22003): BIGINT UNSIGNED value is out of range in '(`Q`.`iu` - 100)'
    engine  -93

Found by review during #0064. The engine holds an i64 and answers, which is why #0064 leaves the UNSIGNED flag off a subtraction: describing the column unsigned while sending -93 would have the text protocol say -93 and the binary one hand the same bytes to a client reading 18446744073709551523. Under-reporting the flag keeps the engine's own two protocols agreeing; it does not make either of them match MySQL.

The same applies to a sum or a product that wraps, which crates/engine/src/expr.rs does deliberately with wrapping_add and wrapping_mul.

Doing this properly needs the evaluator to know an operand is unsigned, which it does not: arithmetic sees only Values and the sign lives on the column. The same shape as #0054 and #0068. Once it lands, `Minus` belongs back in the list in `unsigned_answer`.

## Notes

### 2026-08-21T15:12:48Z — Christopher Vachon (user)

Wider than subtraction: iu + -100 and iu * -1 raise 1690 in MySQL just as iu - 100 does, so unsigned_answer requires every operand to be provably non-negative rather than just excluding Minus. Once this lands, that whole guard comes off.

### 2026-08-25T08:17:18Z — Christopher Vachon (user)

Done in 3b6a342. Value::Unsigned(u64), tagged in coerce because the sign is the column's. A range check alone could not do it: SELECT 18446744073709551615 - 1, SELECT ~0 and storing u64::MAX in a BIGINT UNSIGNED are all things MySQL answers, so refusing them trades one wrong answer for another -- and the check needs a u64 domain, which only a tag on the value supplies.

The check is per operator, and this is the one place #0052's lesson does not transfer: iu - 100 + 200 raises 1690 naming (iu - 100) even though 107 fits. #0052's mistake was checking against the wrong domain (i64), not checking early. Measuring also caught the signed side wrong the same way, where wrapping_add and wrapping_mul were deliberate: 9223372036854775807 + 1 is 1690, not a wrap.

Neither #0068's propagation list nor #0061's transfers, measured fresh again. MOD follows its left operand only where DIV follows either. A conditional keeps the sign only when every candidate has it, but a NULL candidate and a missing ELSE do not count against it. A window function's default is a candidate and its offset is not. A union keeps it where both branches agree, and widens mixed BIGINT signs to NEWDECIMAL keeping every digit. -bu raises the signed spelling of the error, not the unsigned one.

NO_UNSIGNED_SUBTRACTION is honoured now that there is something to honour, through the statement scope REAL_AS_FLOAT already uses. It takes only - out of the unsigned domain: iu + -100, iu * -1 and iu DIV -1 still raise 1690, and bu - 1 becomes a signed 1690.

FORMAT_VERSION 7 -> 8, tag 10 appended. This closes #0077.

Review found two protocol splits and a storage bug: a CASE whose only unsigned candidate is the ELSE was described signed while the value stayed unsigned; a table value constructor sent eight U64 bytes under a VAR_STRING code; and an ENUM read an unsigned number as text, so 1 & 1 and 1 stored different members. The constructor fix widened its column derivation from first-row to all-rows, which is what MySQL does -- the first attempt broke an existing VALUES ROW(TRUE), ROW(NULL) case, which is how the missing NULL filter was caught.

keeping_unsigned was deleted rather than kept: two live call sites reached it but its only non-integer arm was unreachable, which neither cargo machete nor clippy can see structurally, and its doc claimed to be what held the wire flag and the value together. The early returns it duplicated compute the answer exactly in u128 where it could only re-tag what an f64 had already rounded.

60 mutation invocations, 55 distinct, all bite. Corpus 626 -> 870, baseline empty.
