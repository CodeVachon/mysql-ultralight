---
id: 115
title: GREATEST and LEAST do not coerce their answer to the aggregated type
state: Todo
parent: 9
labels: [executor]
created: 2026-08-25T23:13:29Z
updated: 2026-09-03T14:01:18Z
---

## Description

MySQL aggregates the argument types and converts the answer to the result, where this engine hands back the argument it picked, unchanged. Measured on 8.3:

    SELECT GREATEST(1.5, 2)     mysql 2.0    engine 2
    SELECT LEAST(1.50, 2)       mysql 1.50   engine 1.50

The type it reports is `decimal(2,1)` for the first, so 2 comes back as a DECIMAL at one place rather than as the BIGINT it was written as. The second agrees only because the argument that won already had the aggregated scale.

The same question the temporal precision work in #0054 answered for these functions — `computed_column_type` already aggregates their *type* — but the value is not converted to it, so the two can disagree. That is the pairing the binary protocol splits on: a column described NEWDECIMAL with an `Int` behind it is sent as digits by one protocol and by `reconcile`'s fallback in the other.

Found while measuring the function-by-function scale rules for #0070.

## Notes

### 2026-09-03T14:01:18Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. GREATEST(1.5,2) mysql 2.0 engine 2; LEAST(1.50,2) agrees. Type is NEWDECIMAL on both, as the description says -- what differs is the value and the metadata: Length 4 vs 20, Decimals 1 vs 0.
