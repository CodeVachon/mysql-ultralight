---
id: 122
title: A decimal embedded in a document keeps its internal scale, not its displayed one
state: In Progress
parent: 9
labels: [compat, app-priority-2]
created: 2026-08-26T03:49:12Z
updated: 2026-09-03T16:32:40Z
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
