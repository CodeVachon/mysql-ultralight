---
id: 46
title: JSON doubles outside a middling range are written differently
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T15:42:50Z
updated: 2026-08-21T18:30:10Z
---

## Description

Found by review while fixing #0037. JSON scalars are rendered by serde_json, whose number formatting differs from MySQL's in two ways.

The exponent sign: serde writes 1e+30 where MySQL writes 1e30.

The threshold: serde switches to exponent form earlier. MySQL stays in fixed notation while the exponent is in [-15, 14] and uses exponent form outside it - measured, 1e14 is 100000000000000.0 and 1e15 is 1e15; 1e-15 is 0.000000000000001 and 1e-16 is 1e-16. So 1e-7 is 0.0000001 in MySQL and 1e-7 here.

Neither Rust's own Display nor serde matches: Display gives 1000000000000000000000000000000 for 1e30 and drops the .0 from 2.0, which MySQL keeps.

Not urgent. Every number the target codebase stores in a JSON column is well inside [-15, 14], and the differential corpus now carries a case that keeps the divergence visible. The fix is a formatter that reproduces MySQL's rule rather than one that happens to agree on the values tried.

## Notes

### 2026-08-21T18:07:21Z — Christopher Vachon (user)

Both lines fixed; baseline 4 to 2. Rust's own two formatters turn out to be exactly MySQL's two forms — {:e} is the shortest mantissa with a bare exponent, which is the spelling outside the window, and Display is the fixed form inside it. The rule is just picking between them at an exponent of -15 and 14, plus a .0 for a whole double. The 1e-30 case that looked like a precision difference is not one: serde's parse and Rust's shortest form both land where MySQL does, on 9.999999999999999e-31.

### 2026-08-21T18:30:10Z — Christopher Vachon (user)

Review found the upper bound is not an exponent test. MySQL's my_gcvt takes the fixed form when decpt >= -14 and (decpt <= 15 or len > decpt) — so above the threshold a value stays written out while its digits reach past the point, which is why 1e15 is exponential and 1000000000000000.5 is not at the same exponent. The predicate is transcribed rather than approximated now. Corpus gained six JSON cases and six union ones; the one MySQL answers two ways is left out and recorded as #0073.
