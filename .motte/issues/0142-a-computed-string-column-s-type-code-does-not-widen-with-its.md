---
id: 142
title: A computed string column's type code does not widen with its argument
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-31T20:28:22Z
updated: 2026-09-04T00:19:03Z
---

## Description

Found while implementing #0130, pre-existing and general.

    HEX(bl) over a BLOB column    mysql MEDIUM_BLOB   engine VAR_STRING
    HEX(json)                     mysql LONG_BLOB     engine VAR_STRING
    CONCAT(tx,tx) over a TEXT     same shape, already diverged before #0130
    UPPER(tx) over a TEXT         same

MySQL picks a string function's result type code from the argument's maximum length -- Item::max_length arithmetic -- so a function over a BLOB or a TEXT returns a blob code rather than VAR_STRING. This engine returns VAR_STRING for all of them.

The five functions #0130 added join the existing gap rather than widening it, which is why none of their BLOB-argument shapes are in the corpus.

Same underlying mechanism as the computed-width gap #0111 records and crates/differential/README.md documents as deliberately uncompared (2784 divergences over 560 statements when it was measured). Whoever models Item::max_length for widths gets this for free, so the two should be taken together.

## Notes

### 2026-09-03T14:02:14Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed exactly, all four: HEX(bl) mysql MEDIUM_BLOB, HEX(js) mysql LONG_BLOB, CONCAT(tx,tx) and UPPER(tx) mysql MEDIUM_BLOB -- every one VAR_STRING here.

### 2026-09-04T00:19:02Z — Christopher Vachon (user)

Landed in 088b2b7. Type code comes from the item's BYTE width: VAR_STRING to 65535, MEDIUM_BLOB to 16777215, LONG_BLOB above. Widths are counted in characters and multiplied by 4, then multiplied AGAIN for a blob-coded answer but not for one already in bytes -- which is why UPPER over a TEXT is 1048560 and not 262140.

Per-function rules measured over 13 shapes: HEX doubles bytes not characters, UNHEX rounds up, TO_BASE64 wraps every 76 characters, REPLACE grows by the replacement's length independently of the pattern.

Re-measuring for the review fixes caught three bugs the review had not, one of them a wrong type code this change had introduced (UNHEX/FROM_BASE64 widths multiplied by a character set they do not use).
