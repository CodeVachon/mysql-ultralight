---
id: 86
title: A SET in a numeric context is its bitmask
state: Todo
parent: 9
labels: [executor]
created: 2026-08-25T03:02:00Z
updated: 2026-08-25T03:02:00Z
---

## Description

The sibling of #0068, deliberately left out of it.

MySQL reads a SET as a bitmask over the declared members, 1 << position:

    SET('a','b','c') holding 'a,c'   SELECT s + 0   mysql 5   engine 0
                                     SELECT s = 5              mysql 1   engine 0
                                     SELECT s = 'a,c'          mysql 1   engine 1

#0068 kept this out on purpose: a bitmask over several members is a different conversion from a position, with its own assignment rules (comma splitting, dedup, canonical ordering by declared position), its own comparison, and FIND_IN_SET. Bundling the two would have doubled that change and put two conversions behind one FORMAT_VERSION bump.

The shape to follow is #0068's, which is now the worked example: a tagged Value variant written in types.rs::coerce where the declared members are already in hand, an appended image tag with a FORMAT_VERSION bump, numeric readings returning the mask and string readings the canonical text, and the propagation sites measured rather than assumed -- #0068 found that COALESCE/IFNULL/GREATEST/LEAST/MIN/MAX and a scalar subquery drop the tag where IF/CASE/NULLIF keep it, and that a set operation materialises it away entirely. Measure whether SET does the same before copying that list.

Note 4e3b630 already materialises a SET column away through a set operation, alongside ENUM.
