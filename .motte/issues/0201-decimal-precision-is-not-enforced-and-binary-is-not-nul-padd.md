---
id: 201
title: DECIMAL precision is not enforced and BINARY is not NUL-padded
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T17:51:08Z
updated: 2026-09-04T01:56:12Z
---

## Description

Two declared-width gaps #0194/#0195 did not cover, both measured on 8.3.

    INSERT 999999999.99 INTO a DECIMAL(10,2)   mysql 1264 out of range   engine stores it
    BINARY(3) given 'a'                        mysql 0x610000 (NUL-padded)   engine 0x61

The first is the same class of wrong stored data #0194/#0195 fixed for strings and integers -- a DECIMAL's precision is a declared width like any other, and the target application declares 155 DECIMAL columns, which is why this is priority 2 rather than 3.

The second means a fixed-width binary column does not round-trip its declared width.

## Notes

### 2026-09-04T01:56:11Z — Christopher Vachon (user)

Landed in e7ee480, gate green, differential 4526/0. BINARY padding is NOT cosmetic: with BINARY(3) holding 'a', 8.3 answers WHERE b = 'a' with NO rows and WHERE b = 0x610000 with one, and inserting 0x610000 beside 'a' is 1062. So this was a wrong answer to an ordinary query.

DECIMAL's range is read off the RESCALED value -- 99999999.995 into DECIMAL(10,2) is 1264 because it rounds up to nine integer digits. Strictness decides the shape (refuse vs clamp to all-nines) EXCEPT that CAST clamps under every mode.

Storing the NULs broke the duplicate-key message: it rendered them raw, so a C-string client truncated at 'Duplicate entry ''a' and lost the key name. Fixed by rendering through the column's type; the VARBINARY half is filed as #0217.
