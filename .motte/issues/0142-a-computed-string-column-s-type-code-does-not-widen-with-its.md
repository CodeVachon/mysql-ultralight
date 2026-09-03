---
id: 142
title: A computed string column's type code does not widen with its argument
state: Todo
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-31T20:28:22Z
updated: 2026-09-03T14:20:29Z
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
