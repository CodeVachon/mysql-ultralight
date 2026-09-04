---
id: 234
title: information_schema columns report the wrong Org_table and no NO_DEFAULT_VALUE
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-04T04:38:06Z
updated: 2026-09-04T04:38:06Z
---

## Description

Found while implementing #0205/#0206, affecting every column of every information_schema table this engine serves.

    NO_DEFAULT_VALUE   set on 8.3's NOT NULL columns, unset here (every column)
    Org_table          8.3 reports the DATA-DICTIONARY table behind the view
                       -- 'foreign_keys', 'schemata', 'catalogs', 'tables', or empty --
                       where the engine reports the information_schema table's own name

Both are consequences of the same thing: MySQL 8's information_schema is a set of VIEWS over the data dictionary, and this engine builds the rows directly. #0148 already had to model one consequence (three different collations across the columns) and #0111's note records another (COLUMN_NAME is an item reporting 31 decimals where tables.TABLE_NAME is a field reporting 0).

Recording it as one issue because a client reading column metadata sees all three together.
