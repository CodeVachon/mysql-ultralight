---
id: 202
title: UPDATE IGNORE does not parse
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T17:51:08Z
updated: 2026-09-03T17:51:08Z
---

## Description

Found while implementing #0194/#0195.

    UPDATE IGNORE t SET i = 300
    mysql   truncates and clamps, warning only
    engine  1146 Table 'IGNORE' doesn't exist

sqlparser reads IGNORE as the table name. The engine now honours INSERT IGNORE's leniency correctly (#0194/#0195), so this is the one spelling of the same leniency that cannot be reached at all.

Same shape as #0178 (DROP KEY / DROP CHECK): a parser gap rather than an engine gap.
