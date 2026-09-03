---
id: 86
title: A SET in a numeric context is its bitmask
state: Todo
parent: 9
labels: [executor]
created: 2026-08-25T03:02:00Z
updated: 2026-09-03T14:01:17Z
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

## Notes

### 2026-08-31T16:12:58Z — Christopher Vachon (user)

Re-measured during #0087, confirming the bitmask rule with a wider declaration: SET('a','bb','ccc') holding 'bb' is 2 on 8.3 and 'a,ccc' is 5. The engine answers 0 for both.

#0087 added a 'a set column' group to the differential corpus -- the corpus previously had no SET column at all, which is why half of #0087 had nothing to prove it. That group deliberately excludes the numeric context, since it would put a divergence in a run that must end empty. Those cases are the natural place for this issue's coverage to land once it is fixed.

#0087 also established what a SET column's metadata is: MYSQL_TYPE_STRING with SET_FLAG, length (all members + commas) x 4, and the flag is kept through a derived table, CTE, view, generated column and CTAS copy but lost through a UNION.

### 2026-09-03T14:01:17Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. SET bitmask confirmed: s+0 mysql 5 engine 0; s=5 mysql 1 engine 0; s='a,c' agrees. Wider declaration confirmed too: SET('a','bb','ccc') holding 'bb' is 2 and 'a,ccc' is 5 on 8.3, both 0 here. Also noticed: ORDER BY s orders by the mask on 8.3 and alphabetically here.
