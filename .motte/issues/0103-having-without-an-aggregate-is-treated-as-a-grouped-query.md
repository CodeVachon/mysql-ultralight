---
id: 103
title: HAVING without an aggregate is treated as a grouped query
state: Done
parent: 9
labels: [executor]
created: 2026-08-25T14:48:58Z
updated: 2026-09-01T03:03:30Z
---

## Description

Found while implementing #0058, pre-existing and unrelated to it.

    SELECT b FROM J2 HAVING b > 1   mysql answers rows   engine 1055 ONLY_FULL_GROUP_BY

A HAVING with no aggregate and no GROUP BY does not group the query in MySQL; every column stays available and the clause filters rows. This engine treats the presence of HAVING as making the query grouped, so an ordinary column becomes a non-grouped reference and ONLY_FULL_GROUP_BY refuses it.

Refusing a statement MySQL answers, which is the direction that breaks working queries rather than merely disagreeing.

## Notes

### 2026-09-01T03:03:29Z — Christopher Vachon (user)

Done in 4a68f63.

The measured rule: a query is grouped iff it has a GROUP BY, or an aggregate in the select list, or an aggregate in the HAVING -- and an aggregate inside a subquery belongs to that subquery. The HAVING clause itself groups nothing. An aggregated query with no GROUP BY is 1140, not the 1055 this engine gave; with a GROUP BY present it stays 1055.

The routing was the small half. HAVING does not read the FROM clause: a name resolves against the select list's names and aliases, the columns it projects -- by identity rather than spelling, so SELECT b AS zzz ... HAVING b > 1 answers -- the GROUP BY columns, and any enclosing query. Anything else is 1054 in 'having clause', independent of ONLY_FULL_GROUP_BY. A name meaning two things is 1052, a window is 3593 and its alias 3594. Without that guard, loosening the routing would have turned SELECT b FROM t HAVING id > 1 from a refusal into an answer.

Clause order measured and encoded: FROM 1146, field list 1054/1210, WHERE, GROUP BY, HAVING names 1054/1052/3593, HAVING arguments 1210, ORDER BY, then the grouping checks 1055/1140.

Review caught the first cut computing windows before the filter -- a refusal turned into a wrong answer. ROW_NUMBER() over two surviving rows numbered them 2 and 3 where MySQL says 1 and 2, and COUNT(*) OVER () answered 4 against 2. MySQL computes a window over the rows the HAVING kept and draws its partitions over them too. The filter now runs before the projection, which is sound precisely because a HAVING may not name a window at all.

No single-line mutation can express that ordering, because the bug was a call on the wrong side of a statement boundary, and the agent declined to fake one. What pins it instead: the tests assert values chosen so the row set is identical under both orders and only the window value separates them -- 2 against 4, 1,2 against 2,3, 5 against 10 -- plus nine corpus cases putting the same discrimination in front of 8.3 in both protocols. A test asserting only the rows would have passed the bug, which is how it got through the first review.

21 mutations, all bite. Corpus 1904 -> 2030, baseline empty, both gates verified independently.

Left as gaps, none a wrong answer: a window on the grouped path refuses (1235) where 8.3 answers -- confirmed pre-existing by stashing and re-measuring, filed as #0147; ORDER BY COUNT(*) is 3029 there and 1235 here; and the 1140/1055 message text follows this engine's existing style rather than MySQL's wording, which the harness does not compare.
