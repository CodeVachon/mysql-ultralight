---
id: 94
title: EXTRACT's compound units do not parse
state: Done
parent: 9
labels: [compat]
created: 2026-08-25T13:13:52Z
updated: 2026-09-03T13:04:56Z
---

## Description

Found while implementing #0057, which answers the nine units sqlparser can hand it.

sqlparser 0.62 has no keyword for the eleven compound units -- YEAR_MONTH, DAY_HOUR, DAY_MINUTE, DAY_SECOND, DAY_MICROSECOND, HOUR_MINUTE, HOUR_SECOND, HOUR_MICROSECOND, MINUTE_SECOND, MINUTE_MICROSECOND, SECOND_MICROSECOND -- and MySqlDialect::allow_extract_custom() is false, so the statement fails at parse time with a 1064 naming the field. MySQL answers all eleven.

The packing rules are already measured and written down in #0057's note and in crates/engine/src/temporal.rs's neighbourhood, so the evaluation is not the work: every field is two digits wide except microseconds (six) and the year (four), and the sign rides on every clock field and none of the calendar ones. EXTRACT(DAY_SECOND FROM '2024-01-05 06:07:08') is 5060708.

The same hard parser constraint dialect.rs already documents applies -- a wrapper Dialect loses 94 gated features, so this cannot be fixed that way. The routes are a token-level rewrite of the kind 8bacd5d built for infix MOD, or an upstream change to sqlparser.

Note the unit set EXTRACT accepts is exactly the set INTERVAL accepts, verified on 8.3 across all 20 plus 9 non-MySQL names, so whatever fixes one should be checked against the other.

## Notes

### 2026-09-03T13:04:56Z — Christopher Vachon (user)

Landed in a1e2327. Premise held exactly: sqlparser 0.62 has no DateTimeField for any of the eleven compound units and MySqlDialect overrides neither extract-custom flag, so the statement never reached the executor.

The packing is arithmetic, not fixed-width -- YEAR_MONTH of '0999-03-04' is 99903, not 099903 -- which is the kind of thing that would have been wrong quietly if assumed. Only YEAR_MONTH is calendar-side; over a TIME there is no day at all, and the sign rides on all ten clock compounds and none of YEAR_MONTH.

The unit is carried in by a token rewrite; a review caught the internal call name leaking into three client-visible places (described column, ONLY_FULL_GROUP_BY walk, VIEW_DEFINITION). Stored view text keeps the rewritten form because the catalog re-parses it.

11 mutations, two chosen to span four units each so a table-driven test cannot pass on one row. Divergences filed as #0186, #0187.
