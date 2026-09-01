---
id: 144
title: ORDER BY resolves a shadowed column before a select-list alias
state: Done
parent: 9
labels: [executor]
created: 2026-09-01T02:16:08Z
updated: 2026-09-01T20:08:23Z
---

## Description

Found by new corpus cases while implementing #0103, and reproduces with no HAVING anywhere.

    SELECT id AS b FROM Hv ORDER BY b
    mysql   sorts by the alias   1,2,3,4
    engine  sorts by the column b   4,1,2,3

crates/engine/src/exec.rs's sort_rows runs against the relation before the projection exists, so a name that is both a select-list alias and a real column of the table resolves to the column. MySQL resolves an ORDER BY name against the select list first and only then the FROM clause.

A wrong answer rather than a refusal, and a quiet one -- every row is present and plausibly ordered.

Related to #0090, which is the positional form of the same clause resolving against the wrong thing (ORDER BY 1 sorts by the input column rather than the projection). Whoever takes one should take the other: both are sort_rows running too early to see what the query actually projects.

The corpus case that found this was reshaped so the baseline could stay empty, with the reason written above it.

## Notes

### 2026-09-01T20:08:23Z — Christopher Vachon (user)

Done in 613ae1d, with #0090.

The old comment was not simply wrong, which is why this is a precedence rule rather than a swap: MySQL does let an ORDER BY name a column the select list does not return, and that still works.

Measured rules. A bare unqualified name takes the select list first and the relation second; the same name inside a larger expression takes the relation first, so ORDER BY b is the alias and ORDER BY b+1, -b or ABS(b) is the column. A qualified name never sees an alias. The select list is scanned left to right and an item that is not a relation column wins outright and stops the scan, so SELECT id AS b, 1 AS b, s AS b ORDER BY b sorts on the 1; two different columns before that point are 1052 and two spellings of the same column are not.

That last rule was settled by measurement twice. The first reading had a column beating an expression, and MySQL's rows agreed under ASC and disagreed under DESC -- the obvious test direction would have confirmed the wrong rule.

Also measured and encoded: an alias is invisible to WHERE (1054), visible to HAVING, and shadowed in GROUP BY; DISTINCT with a sort key the select list does not carry is 3065, though a position, a bare name the list answers, and the item's expression written again are all fine; an aggregate in the ORDER BY of an ungrouped query is 3029.

Review found two regressions the change introduced. A shadowed alias in a grouped query fell through to a raw relation column read off an arbitrary row -- 8.3 refuses that with 1055, on the column the clause reads rather than the spelling, so the grouping check now runs over the resolved keys, after the select list so that is still reported first. And a USING-coalesced wildcard column bound to an unrelated aliased item sharing its name, sorting by the wrong expression; restricting the search to each wildcard's own expansion also made a qualified wildcard non-coalescing, which is what 8.3 does.

The escape guard needed hoisting here too -- the third clause where a check in the row path did not run when there were no rows, after #0058 and #0093. A union or FROM-less select answered where 8.3 raises 1210, invisible until the result was empty because a non-empty union raises it through evaluation.

#0103's trap was guarded directly: the sort still runs before the projection, so a window is numbered over the sorted rows, asserted by a test whose rows are identical either way and only the window value separates them.

24 mutations, all bite, including one that swaps the two resolutions rather than removing either. Corpus 2080 -> 2288, baseline empty, both gates verified independently.
