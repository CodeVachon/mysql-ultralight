---
id: 145
title: A string function mangles non-UTF-8 bytes through a lossy decode
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-01T02:16:09Z
updated: 2026-09-03T14:20:29Z
---

## Description

Found by review during #0103, and pre-existing -- LENGTH(X'FF') was already 3.

Every string function that reads its argument through as_text() replaces a non-UTF-8 byte with U+FFFD:

    LENGTH(UNHEX('FF'))            mysql 1      engine 3
    HEX(CONCAT(UNHEX('FF')))       mysql FF     engine EFBFBD
    HEX(CONCAT(UNHEX('FF'),'B'))   mysql FF42   engine EFBFBD42
    LENGTH(FROM_BASE64('/w=='))    mysql 1      engine 3

Same for LOWER, TRIM, SUBSTRING, REVERSE and CAST(... AS CHAR).

What makes it worth doing now is #0130: UNHEX and FROM_BASE64 are the first way ordinary portable SQL produces such bytes. Before them a client had to write a hex literal or cast to BINARY. No corpus case composes the new functions with an older one, which is why the harness is green over it -- so the Value::Bytes tag #0130 added should not be read as having closed this.

The fix is for the string functions to operate on bytes when given bytes, which is the same shape as #0074's SOUNDEX split and #0128's charset propagation. Those three plus #0139, #0140 and #0143 are one mechanism seen from six sides.

## Notes

### 2026-09-03T14:05:52Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed exactly, all four: LENGTH(UNHEX('FF')) 1 vs 3, HEX(CONCAT(UNHEX('FF'))) FF vs EFBFBD, HEX(CONCAT(UNHEX('FF'),'B')) FF42 vs EFBFBD42, LENGTH(FROM_BASE64('/w==')) 1 vs 3. EFBFBD is U+FFFD, so the bytes are being decoded lossily and re-encoded.
