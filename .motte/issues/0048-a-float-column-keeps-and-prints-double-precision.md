---
id: 48
title: A FLOAT column keeps and prints double precision
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T15:57:59Z
updated: 2026-08-21T20:05:04Z
---

## Description

Split out of #0047. A FLOAT column holds four bytes in MySQL and prints about six significant digits: storing 1.234567890123 and reading it back gives 1.23457. This engine keeps the full double and prints 1.234567890123.

Rounding the stored value to f32 is one line and makes the value right, but it makes the *rendering* worse rather than better - 1.2345678806304932 - because Value::Float does not know the column it came from and renders every double the same way. Getting it right needs the column's type at render time.

Left alone deliberately, which is why FLOAT keeps its MYSQL_TYPE_DOUBLE wire type in #0038: declaring MYSQL_TYPE_FLOAT would make the binary protocol write four bytes for a value the engine stored as eight. FLOAT does not appear in the target schema, which uses DOUBLE and DECIMAL.

## Notes

### 2026-08-21T19:43:10Z — Christopher Vachon (user)

Done together with #0055, because they are one mechanism: the rendering rule is my_gcvt's and only the precision differs — as many digits as a double needs, six for a float. Without it the storage fix alone makes things worse, which is what the issue warned. Value::Float32 is tagged like Value::Decimal; it does not survive arithmetic, which is right because MySQL reads f+0 as a double. FORMAT_VERSION goes to 5. Split out #0075 for FLOAT(M,D), which displays fixed decimals and is a different mechanism.

### 2026-08-21T20:05:04Z — Christopher Vachon (user)

Review found the storage fix had split the two protocols for FLOAT *expressions*: the value tag and the declared type are decided in different code, and they disagreed six ways. Measured all of them against 8.3 — MySQL widens -f, ABS(f), ROUND(f,2) and f+0 to a double, and keeps COALESCE(f,0), LEAST(f,9), GREATEST(f,0) and CAST(f AS FLOAT) a float. Fixed the three type rules to match, and added ResultSet::reconcile so the class is closed rather than the list. Also: a FLOAT past FLT_MAX stored an infinity where MySQL raises 1264, and the new exponent form leaked into NEWDECIMAL columns.
