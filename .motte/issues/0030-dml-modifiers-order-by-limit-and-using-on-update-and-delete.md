---
id: 30
title: "DML modifiers: ORDER BY, LIMIT and USING on UPDATE and DELETE"
state: Done
parent: 6
labels: [executor]
created: 2026-08-18T21:18:33Z
updated: 2026-08-19T19:49:51Z
---

## Description

Parsed and refused today with a clear 1235, because dropping them silently turns a one-row statement into a whole-table one: `DELETE FROM t LIMIT 1` emptied the table before this was caught.

Needed: ORDER BY and LIMIT on single-table UPDATE and DELETE, and DELETE ... USING. None appear in the 605 migrations or the provisioning dumps, which is why they are refused rather than implemented, but they are ordinary MySQL and a test suite may well use them.

## Notes

### 2026-08-19T19:49:51Z — Christopher Vachon (user)

Landed. ORDER BY and LIMIT on single-table UPDATE and DELETE, and DELETE ... USING.

The row loops now settle which rows to touch before writing any of them: a match pass, then dml_row_order sorts and truncates, then the write pass. The predicate only ever reads its own row and subqueries resolve against the committed catalog rather than the working copy, so the split is equivalent to the old interleaved pass.

ORDER BY is honoured with no LIMIT too, because it sets the order rows are written in: UPDATE Seq SET id = id + 1 ORDER BY id DESC succeeds where the unordered and ASC forms both hit 1062. That is the regression test for the sort.

DELETE ... USING: MySQL spells multi-table delete two ways that disagree about where the targets live, so both now reach delete_joined with the targets pulled out first. build_relation_list was extracted from the SELECT path so USING a, b cross-joins the way FROM a, b does.

Multi-table UPDATE and DELETE still refuse both modifiers, as MySQL does - the first row is not defined across a join.

Bugs found while probing and in review, all of them the same shape as the original: a clause read for less than it says.

- DELETE Widget FROM Widget, Tag USING Widget JOIN Tag named one target before FROM and two after it, and deleted from both - emptying a table nobody named. MySQL rejects the spelling; so does the engine now.
- LIMIT coerced rather than requiring a row count: LIMIT 1.9 removed one row, LIMIT '2' removed two. Now an exact non-negative integer or 1064. LIMIT TRUE is still accepted as exactly 1, which is a superset of MySQL but not a rounding.
- DELETE t1 FROM t1, t2 WHERE ... - the commoner spelling - was still refused with 1235 while the rarer USING form worked, purely because a from.len() != 1 guard ran first.
- The delete partition hashed a set and cloned every doomed row, so DELETE FROM t with no WHERE allocated a second copy of the table. One pass over a position map now.
- docs/prototype.md still listed these, and the #0029 functions, under 'What it does not do yet' beneath a line claiming the engine errors for each. Corrected in both places.

226 engine tests, each mechanism verified by breaking it and watching the tests fail. 604 migrations still apply, conformance unchanged at 99.9 percent.
