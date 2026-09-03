---
id: 124
title: A shortest-form tie rounds half up where MySQL rounds half to even
state: Todo
parent: 9
labels: [compat]
created: 2026-08-26T07:20:32Z
updated: 2026-09-03T14:01:18Z
---

## Description

Measured while closing #0072, and a different mechanism from that issue -- it is about writing a double, not reading one, and it is not confined to JSON.

    SELECT CAST('694593818398461.3' AS DOUBLE)   mysql 694593818398461.2   engine ...3
    SELECT 694593818398461.3e0                    mysql 694593818398461.2   engine ...3
    SELECT CAST('[694593818398461.3]' AS JSON)    mysql [694593818398461.2] engine [...3]

Both sides land on the same double; they disagree about which shortest decimal names it. Gay's dtoa, which MySQL uses, breaks a shortest-representation tie to even; Rust's flt2dec breaks it up. In all 414 observed cases MySQL's last digit is even and the engine's is exactly one higher.

Rate: 1.16% of 35669 mixed JSON literals; on the DOUBLE path 0.4-0.9% of random 16-19 digit inputs, and 9.1% of two-decimal literals in [1e15, 4.5e15) -- which is the range #0072 was originally recorded against, and is why the two were conflated.

Fixing it means a shortest-round-trip formatter with an even tie-break, shared by json::double and the engine's DOUBLE rendering. Fixing only one would make the engine disagree with itself, which is why #0072 did not fold it in.

No differential case can exist for this while the baseline must stay empty -- every statement that exposes it diverges today. That is the argument for fixing it rather than recording it.

## Notes

### 2026-09-03T14:01:18Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed exactly, all three forms: CAST('694593818398461.3' AS DOUBLE), the 694593818398461.3e0 literal, and CAST('[...]' AS JSON) are ...461.2 on 8.3 and ...461.3 here.
