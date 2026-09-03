---
id: 88
title: GROUP_CONCAT ignores its ORDER BY
state: Done
parent: 9
labels: [executor]
created: 2026-08-25T03:02:14Z
updated: 2026-09-03T05:25:26Z
---

## Description

Found by review during #0068, outside that change.

GROUP_CONCAT(x ORDER BY y) concatenates in whatever order the rows arrive; the ORDER BY inside the call is parsed and discarded. MySQL orders the concatenated values by it.

A wrong answer rather than a refusal, and a quiet one -- the result is a plausible string that happens to be in the wrong order, and a caller comparing it against an expected value fails without any indication that the ordering clause was the part ignored. Refusing the clause would at least be honest until it is implemented.

Worth checking SEPARATOR and DISTINCT in the same pass, since they are parsed at the same site, plus group_concat_max_len.

## Notes

### 2026-09-03T05:25:26Z — Christopher Vachon (user)

Done in 57dedb6, in an isolated worktree. Four clauses were being dropped, not one.

ORDER BY was parsed and discarded; every argument after the first was dropped; SEPARATOR was ignored and a comma used; DISTINCT kept arrival order; and group_concat_max_len was stored and never read.

Measured rules. A position inside the call counts the CALL's arguments, not the select list -- GROUP_CONCAT(s, n ORDER BY 2) sorts by n. Parentheses peel: ORDER BY (1) answers sorted while ORDER BY (2) over a one-argument call is 1054 in the order clause. An unresolvable name is 1054 'field list', or 'having clause' when written there.

DISTINCT without ORDER BY dedupes and then SORTS BY VALUE (a,B and 9,10,100); with an ORDER BY it keeps the FIRST row of each value and sorts on that row's key.

The separator's grammar is wider than a quoted string -- 0x2D and X'2D' are both a hyphen, 0x2D2D is two -- so it goes through expr::literal rather than the written text. b'101101' has no value anywhere in this engine (#0141), so it refuses rather than joining with the characters B'101101'.

group_concat_max_len defaults to 1024, truncates in BYTES but never through a character, and clamps to a minimum of 4 -- which closes the one line #0095 names for this variable. No 1260/1292 warning, because the engine has no warning channel.

A tie on the sort key is deliberately not matched: 8.3 puts the later row first and this engine's sort is stable. Documented, and nothing in the suite or corpus ties.

Review moved the window refusal out of compute_window into refuse_at_resolve, where 8.3 settles it -- five shapes that had answered now refuse: FROM-less, untaken CASE, WHERE 1=0, empty table, ORDER BY. That is the zero-rows checklist applied unprompted. Ordering measured: 1582 > 1210 > 1054 > 1235.

The agent corrected its own reviewer with measurement: the reviewer stated as fact that ORDER BY (2) should sort; 8.3 answers 1054. There is a corpus case behind it now.

13 mutations, all bite. Corpus 3000 -> 3072, baseline empty, both gates verified independently on the merged tree.

Left and filed/known: GROUP_CONCAT is typed LONG_BLOB regardless of the limit where 8.3 sends VAR_STRING at <= 512 (computed_column_type has no session; every corpus limit is above 512 so no case hides it); named windows are unsupported for every function, so 8.3's group_concat refusal arrives as the engine's named-window one.
