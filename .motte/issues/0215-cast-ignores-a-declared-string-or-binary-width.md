---
id: 215
title: CAST ignores a declared string or binary width
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-04T01:36:11Z
updated: 2026-09-04T04:20:26Z
---

## Description

Found while implementing #0201, measured on 8.3. One gap with two spellings.

    HEX(CAST('a' AS BINARY(3)))      mysql 610000 (padded)   engine 61
    HEX(CAST('abcd' AS BINARY(3)))   mysql 616263 (truncated) engine 61626364
    CAST('abcd' AS CHAR(3))          mysql 'abc'             engine 'abcd'

Bare CAST('abc' AS BINARY) agrees, so it is specifically the declared width that functions::cast ignores.

#0201 fixed CAST for DECIMAL, where the task asked for it, and deliberately did NOT half-fix this one: CAST truncates where a column write refuses (1406), so the two paths have different semantics and this needs its own measurement rather than reusing the write path's rule.

Priority 2 because the target application calls CAST 27 times.

## Notes

### 2026-09-04T04:20:26Z — Christopher Vachon (user)

Landed in 67e5cfe. #0201's deferral was RIGHT: a cast needs its own machinery because all three write-path rules are wrong for it.

    too wide         write refuses 1406 under strict, else truncates + 1265
                     cast truncates under EVERY mode incl. TRADITIONAL, + 1292
    trailing spaces  write exempts them, keeps them, raises a Note
                     cast has no exemption, cuts them, raises a Warning
    CHAR(n)          write pads in and strips out; cast does neither

CHAR(n) counts characters, BINARY(n) counts bytes and pads with NULs, so CAST('eee' AS CHAR(2)) is 4 bytes while CAST('eee' AS BINARY(3)) cuts mid-character. The truncation message carries the byte length of what was KEPT, not the declared count.

One mutation passed and was reported rather than hidden: removing the byte cut left the value right (the padding helper's resize also shrinks) and lost only the warning, so it bites against the warnings test instead.
