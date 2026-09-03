---
id: 105
title: A hex literal column is described signed
state: Done
parent: 9
labels: [compat]
created: 2026-08-25T16:39:34Z
updated: 2026-09-03T09:22:20Z
---

## Description

Found by the new naming corpus group in #0059, and unrelated to naming -- that change touches only render_expr.

    SELECT X'41'   mysql UNSIGNED   engine signed
    SELECT 0x41    mysql UNSIGNED   engine signed

Four columns each across the text and binary protocols. The two corpus statements were narrowed rather than blessed, with the finding recorded in a comment at that spot in cases.rs, so the baseline stays empty.

Note 3b6a342 gave the engine Value::Unsigned and the machinery to describe a column unsigned, so this is now a matter of typing the literal rather than a missing mechanism.

## Notes

### 2026-09-03T09:22:20Z — Christopher Vachon (user)

Done in ef881a8 (with #0106).

Measured: X'41', x'41' and 0x41 are VAR_STRING, binary charset, length in BYTES, flags NOT_NULL UNSIGNED BINARY. A bit literal b'101' is the same WITHOUT unsigned. Nothing moves under ANSI, TRADITIONAL or NO_UNSIGNED_SUBTRACTION.

The real work was where the flag travels, and the families disagree. Keeping it: parentheses at any depth, a unary plus, a scalar subquery, NULLIF (following its first argument), MIN, MAX, ABS (which is DOUBLE and unsigned) and MOD. Losing it: COALESCE, IFNULL, GREATEST, LEAST, IF, CASE, and the window positional functions LAG/LEAD/FIRST_VALUE/LAST_VALUE/NTH_VALUE.

The rule underneath: the losing set builds a NEW item over a type aggregated across its candidates and takes the sign from that type, so the flag survives only where the aggregate is itself an integer -- COALESCE(iu,bu) is unsigned, GREATEST(iu,'a') is signed. The keeping set hands one item back.

Materialisation is separate again: a UNION or derived table keeps UNSIGNED for an integer and a YEAR and drops it for VAR_STRING. That fixed a real bug -- a derived table over a hex literal was declared varbinary(512) unsigned.

#0141 does not block this. The type code, charset and flags of a hex literal do not depend on reading it as a number; all of SELECT X'41' and its wrappers match 8.3 exactly now. #0141 bites only in a NUMERIC context -- -X'41' is -65 there and -0 here, ABS(X'41') is 65 against 0 -- so those two stay out of the corpus and are asserted on their type alone.

16 mutations, all bite. One earlier attempt passed because no CASE row distinguished it; replaced with one that does.

Left and filed as #0167: MySQL also drops UNSIGNED through a UNION for a DECIMAL UNSIGNED and a FLOAT UNSIGNED, where this engine keeps it. The guard uses is_numeric rather than is_integer on purpose, because the loop below reads the same field to decide whether to untag the values.

Corpus 3284 -> 3310, baseline empty, both gates verified independently on the merged tree -- and the agent's derived count matched the harness exactly, the first time that has held since the count drifted.
