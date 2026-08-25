---
id: 94
title: EXTRACT's compound units do not parse
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T13:13:52Z
updated: 2026-08-25T13:13:52Z
---

## Description

Found while implementing #0057, which answers the nine units sqlparser can hand it.

sqlparser 0.62 has no keyword for the eleven compound units -- YEAR_MONTH, DAY_HOUR, DAY_MINUTE, DAY_SECOND, DAY_MICROSECOND, HOUR_MINUTE, HOUR_SECOND, HOUR_MICROSECOND, MINUTE_SECOND, MINUTE_MICROSECOND, SECOND_MICROSECOND -- and MySqlDialect::allow_extract_custom() is false, so the statement fails at parse time with a 1064 naming the field. MySQL answers all eleven.

The packing rules are already measured and written down in #0057's note and in crates/engine/src/temporal.rs's neighbourhood, so the evaluation is not the work: every field is two digits wide except microseconds (six) and the year (four), and the sign rides on every clock field and none of the calendar ones. EXTRACT(DAY_SECOND FROM '2024-01-05 06:07:08') is 5060708.

The same hard parser constraint dialect.rs already documents applies -- a wrapper Dialect loses 94 gated features, so this cannot be fixed that way. The routes are a token-level rewrite of the kind 8bacd5d built for infix MOD, or an upstream change to sqlparser.

Note the unit set EXTRACT accepts is exactly the set INTERVAL accepts, verified on 8.3 across all 20 plus 9 non-MySQL names, so whatever fixes one should be checked against the other.
