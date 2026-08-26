---
id: 72
title: serde and MySQL round a JSON literal tie differently
state: Done
parent: 9
labels: [compat]
created: 2026-08-21T18:23:38Z
updated: 2026-08-26T07:20:19Z
---

## Description

SELECT CAST('[2000000000000000.25]' AS JSON)
    mysql   [2e15]
    engine  [2000000000000000.3]

Found while closing #0046. This is not the number *formatting* that issue was about — crates/engine/src/json.rs now reproduces my_gcvt's rule exactly, and every value the two parsers agree on is written identically. The disagreement is one step earlier: serde_json and MySQL parse that literal to different doubles, because 17 significant digits at that magnitude land on a tie and the two break it the other way. Rust's own compiler-parsed literal agrees with serde.

Bounded to inputs written with more significant digits than a double carries, at a magnitude where the ulp makes the extra digits a tie — roughly [1e15, 4.5e15) for a two-decimal input. Nothing in the corpus or the target codebase is near it.

## Notes

### 2026-08-26T07:20:19Z — Christopher Vachon (user)

Done in c468adc. The issue's framing was too narrow and conflated two mechanisms; measuring separated them.

Cause A, which is this issue: MySQL parses JSON with rapidjson at its default precision, keeping at most seventeen significant digits and folding the rest one rounding at a time, while serde accumulates into a u64 until it overflows and keeps nineteen or twenty. They agree to fifteen digits and part company above. Not the recorded 'roughly [1e15, 4.5e15) for a two-decimal input' -- a sweep of 1000 literals per shape found about 30-43% of any literal with 18+ significant digits diverging at any magnitude, and about 4% at seventeen. Examples: 987119.34197208915, 123456789012345678901234567890, 4.7944969753680819380e17.

Confined to the JSON path, verified: CAST(str AS DOUBLE), a DOUBLE literal and a DOUBLE column show zero parse divergence, because Rust's parse and my_strtod are both correctly rounded. That is why the fix is confined to json.rs.

Option (a) taken -- reproduce rapidjson -- because a third of all 18-digit literals silently getting a different number is a class rather than a corner, and pinning it under option (b) would have pinned a wrong answer a fix could remove. Validated against 8.3 on 35669 literals across magnitudes and digit counts, plus families built on the 2^53, 2^63 and 2^64 handovers, subnormals, 20-digit exponents and leading-zero fractions: zero parse divergences. Review independently transcribed rapidjson and fuzzed 428266 more, also zero.

Three pieces deliberately not transcribed, each with its reason at the site: a 32-bit pre-accumulator, a lookahead digit at the exact handover, and a guard that is never false. No input makes any of them change the answer, and two of them could not be made to bite under mutation -- which is what identified them.

Review challenged the negative-exponent ceiling as following rapidjson's comment rather than its code. Checked against MySQL 8.3's own bundled copy: the line there is exactly (expFrac + 2147483639) / 10, and the derivation gives it -- exp <= (expFrac - INT_MIN - 9)/10 with -INT_MIN = 2147483648. The reviewer read INT_MAX for INT_MIN and flipped the sign. Kept as written, with the trap now documented at the site so the next reader does not repeat it.

Cause B is separate and stays open as #0124: at a shortest-representation tie MySQL's dtoa rounds half to even and Rust's rounds half up. Not JSON-specific.

18 mutations, all bite. Two needed a covering case added rather than accepted. Corpus 1420 -> 1440, baseline empty.
