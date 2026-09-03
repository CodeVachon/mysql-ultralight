---
id: 174
title: An inline PRIMARY KEY UNIQUE column loses its unique index, and a functional index is named after its expression
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T10:20:07Z
updated: 2026-09-03T10:20:27Z
---

## Description

Two leftovers from the index-naming pass in #0150, both measured on mysql:8.3 and both deliberately out of its scope.

**A column that is both.** `CREATE TABLE t (a INT NOT NULL PRIMARY KEY UNIQUE)` gives 8.3 two indexes — `PRIMARY` and `a` — and gives this engine only `PRIMARY`, because the loop that turns a column-level UNIQUE into an index filters out any column whose `primary_key` flag is set. The table-level spelling `(a INT NOT NULL, UNIQUE (a), PRIMARY KEY (a))` now matches 8.3, since #0150 moved that loop ahead of the constraints; only the inline spelling is left.

The filter cannot just be dropped: with the extra index in place, a duplicate row would have to keep reporting `PRIMARY` rather than `a`, which is what `Table::keys` orders for.

**A functional index.** `INDEX ((a + 1))` is called `functional_index` on 8.3, and a second one in the same table `functional_index_2` — the same suffix rule every invented index name follows, over a fixed base rather than over the first column. This engine names it after the rendered expression, `(a + 1)`. The index does nothing here either way (it matches an index's columns by name and no column is called that), so the name is all there is to it, but it is what `information_schema.statistics` and `DROP INDEX` see.
