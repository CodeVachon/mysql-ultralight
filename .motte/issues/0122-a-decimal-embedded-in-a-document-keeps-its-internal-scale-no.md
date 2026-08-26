---
id: 122
title: A decimal embedded in a document keeps its internal scale, not its displayed one
state: Todo
parent: 9
labels: [compat]
created: 2026-08-26T03:49:12Z
updated: 2026-08-26T03:49:12Z
---

## Description

Found while measuring for #0071, and adjacent to #0070/#0085 rather than part of them.

    SELECT AVG(a)              agrees -- six places both sides
    SELECT JSON_ARRAY(AVG(a))  mysql [3086419.575000000]   engine [3086419.575000]
    SELECT JSON_ARRAY(a/2)     same shape

MySQL's decimal division computes to a nine-digit word boundary and the document takes the raw decimal, while the string conversion rounds to the displayed scale. So the same value has two spellings depending on whether it is printed or embedded, and this engine only produces the printed one.

Same mechanism as #0118, seen through JSON instead of through a column: the value carries more places than its display declares. Whoever takes one should look at the other.
