---
id: 155
title: A supplementary character survives in a generated column name
state: Todo
parent: 9
labels: [compat]
created: 2026-09-02T17:25:27Z
updated: 2026-09-02T17:25:27Z
---

## Description

Found by the harness on the first run of #0140's new group.

    SELECT HEX(UPPER('𐐨'))
    mysql names the column HEX(UPPER('?'))
    engine names it        HEX(UPPER('𐐨'))

MySQL's item names are utf8mb3, so a character outside the BMP cannot be spelled in one and is replaced with a question mark.

Small, and it belongs with the naming work: #0043 named a column after the text it was written as, #0059 named a literal by its contents with a 255-byte cut on a character boundary, and #0060 carried the name back through the rewriter. This is the same rule with one more constraint -- the name is not utf8mb4.

No corpus case asks it today. Worth checking whether the cut in #0059 interacts: a 255-BYTE cut on a name that is then transcoded to utf8mb3 may not land where MySQL's does.
