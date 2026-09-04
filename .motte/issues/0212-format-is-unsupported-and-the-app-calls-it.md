---
id: 212
title: FORMAT() is unsupported, and the app calls it
state: Done
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-03T23:20:40Z
updated: 2026-09-04T04:20:26Z
---

## Description

Found while implementing #0118/#0122/#0123.

    SELECT FORMAT(1/3, 9)
    mysql   0.333333333
    engine  ERROR 1235 does not yet support function 'format()'

Worth noting for whoever implements it: FORMAT reads the decimal's VALUE, unlike ROUND and TRUNCATE which cap at the argument's shown scale. That distinction is now modelled (see decimal.rs and functions.rs::shown_places), so the information is there to do this correctly.

Priority 2 because the conformance scan counts 9 FORMAT calls in application-mono-repo.

## Notes

### 2026-09-04T04:20:26Z — Christopher Vachon (user)

Landed in 67e5cfe (gate green). FORMAT is not ROUND with commas -- three measurements say so: it reads a decimal's VALUE where ROUND/TRUNCATE cap at its shown scale; its rounding belongs to the ARGUMENT (exact half away from zero, approximate half to even, and a STRING takes the approximate path because MySQL picks by declared result type); and the double path is two steps, rint(v*10^p)/10^p then render, visible in FORMAT(0.145e0,2)=0.14 against FORMAT(0.145,2)=0.15.

The locale table was measured, not transcribed: mysqld holds 634 xx_YY strings, SET lc_time_names accepts 112, and each one's point/separator/grouping was read out of HEX(FORMAT(...)) -- the only way to see that fr_FR has NO separator and bg_BG separates with a NUL byte. Nine distinct shapes cover all 112. An unknown locale is warning 1649 plus the en_US answer, not an error.
