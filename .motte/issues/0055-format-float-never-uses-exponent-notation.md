---
id: 55
title: format_float never uses exponent notation
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T19:38:59Z
updated: 2026-08-21T19:43:10Z
---

## Description

Found by review during #0042, and pre-existing — the values are right and only their rendering differs.

    SELECT 1e15 + 0     engine 1000000000000000            mysql 1e15
    SELECT 1e20 + 0     engine 100000000000000000000       mysql 1e20
    SELECT 1e-300 + 0   engine 0.000...0001 (301 digits)   mysql 1e-300
    SELECT 1e-5 + 0     engine 0.00001                     mysql 0.00001

MySQL renders a DOUBLE with up to 15 significant digits and switches to exponent form outside a range; note that the switch is not plain %g, which would give 1e-05 for the fourth row where MySQL gives 0.00001. The thresholds need measuring rather than assuming.

Worth doing because a 301-digit token is what a client reads for 1e-300, and because #0042's clamp of a string conversion to DBL_MAX now renders as 309 digits where MySQL sends 1.7976931348623157e308.

## Notes

### 2026-08-21T19:43:10Z — Christopher Vachon (user)

Done as part of #0048: the two share one formatter, now in the protocol crate where both it and the engine can reach it. Rust's Display never reaches for an exponent, so 1e20 was twenty-one digits and DBL_MAX three hundred and nine; the rule is my_gcvt's, the same one #0046 gave a document's numbers, minus the trailing .0 that a column drops.
