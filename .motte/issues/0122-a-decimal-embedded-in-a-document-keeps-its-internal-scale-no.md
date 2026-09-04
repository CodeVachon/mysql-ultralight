---
id: 122
title: A decimal embedded in a document keeps its internal scale, not its displayed one
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-26T03:49:12Z
updated: 2026-09-03T23:20:39Z
---

## Description

Found while measuring for #0071, and adjacent to #0070/#0085 rather than part of them.

    SELECT AVG(a)              agrees -- six places both sides
    SELECT JSON_ARRAY(AVG(a))  mysql [3086419.575000000]   engine [3086419.575000]
    SELECT JSON_ARRAY(a/2)     same shape

MySQL's decimal division computes to a nine-digit word boundary and the document takes the raw decimal, while the string conversion rounds to the displayed scale. So the same value has two spellings depending on whether it is printed or embedded, and this engine only produces the printed one.

Same mechanism as #0118, seen through JSON instead of through a column: the value carries more places than its display declares. Whoever takes one should look at the other.

## Notes

### 2026-09-03T14:02:14Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3. Divergence HOLDS; the engine value quoted in the description is stale.

    SELECT AVG(a)              agrees (as documented)
    SELECT JSON_ARRAY(AVG(a))  mysql [3086419.575000000]  engine [3086419.57500000]
    SELECT JSON_ARRAY(a/2)     mysql [1543209.787500000]  engine [1543209.78750000]

MySQL carries 9 fractional places; the engine now carries 8, not the 6 the description records. Direction and cause unchanged.

### 2026-09-03T23:20:39Z — Christopher Vachon (user)

Landed in f7be76b as part of the value-vs-display split. The engine value quoted in this description was already stale when validated on 2026-09-03 (8 places, not the 6 recorded); it now matches 8.3's 9.

The mechanism is #0118's: a decimal's value carries its own width and the column shows it at another. This composes with the JSON DECIMAL_MARK from #0121 rather than fighting it -- the mark rides on whatever decimal text is handed to json::from_value, so widening the text widened what gets marked. Re-measured after rebasing: JSON_ARRAY(AVG(a)), JSON_OBJECT('a', 1/3), CAST(1/3 AS JSON), JSON_TYPE, JSON_EXTRACT, JSON_UNQUOTE, document equality and a wide quotient stored in a JSON column and read back -- all agree.
