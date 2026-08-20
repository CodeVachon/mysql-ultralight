---
id: 46
title: JSON doubles outside a middling range are written differently
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T15:42:50Z
updated: 2026-08-20T15:42:50Z
---

## Description

Found by review while fixing #0037. JSON scalars are rendered by serde_json, whose number formatting differs from MySQL's in two ways.

The exponent sign: serde writes 1e+30 where MySQL writes 1e30.

The threshold: serde switches to exponent form earlier. MySQL stays in fixed notation while the exponent is in [-15, 14] and uses exponent form outside it - measured, 1e14 is 100000000000000.0 and 1e15 is 1e15; 1e-15 is 0.000000000000001 and 1e-16 is 1e-16. So 1e-7 is 0.0000001 in MySQL and 1e-7 here.

Neither Rust's own Display nor serde matches: Display gives 1000000000000000000000000000000 for 1e30 and drops the .0 from 2.0, which MySQL keeps.

Not urgent. Every number the target codebase stores in a JSON column is well inside [-15, 14], and the differential corpus now carries a case that keeps the divergence visible. The fix is a formatter that reproduces MySQL's rule rather than one that happens to agree on the values tried.
