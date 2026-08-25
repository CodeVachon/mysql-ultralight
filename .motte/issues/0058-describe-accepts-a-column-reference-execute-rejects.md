---
id: 58
title: describe accepts a column reference execute rejects
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T20:16:48Z
updated: 2026-08-25T15:46:31Z
---

## Description

SELECT a.b FROM J2, where no table or alias `a` is in scope, is refused by MySQL at *prepare* time with 1054 "Unknown column 'a.b' in 'field list'". This engine prepares it happily, reports one column named `b`, and only fails when the statement is executed.

Execution is right; it is the describe path that does not resolve the qualifier. A client that prepares its statements up front — which is what the binary protocol is for — learns about the mistake later than it should, and a test that only prepares learns nothing at all.

Found while measuring column names for #0043.

## Notes

### 2026-08-25T15:46:30Z — Christopher Vachon (user)

Done in 09233ad.

The issue's premise was wrong and the work disproved it: execution was not already right. A name is resolved when a row is evaluated against it, so over zero rows nothing is resolved -- SELECT a.b FROM Empty, WHERE 1=0 and LIMIT 0 all returned a successful empty result set at execute. Describing is the always-zero-rows case, so one check covers both.

Measured and encoded: a qualified wildcard is 1051 Unknown table, not 1054; the qualifier check runs before ONLY_FULL_GROUP_BY, so a bad reference in a grouped query is 1054 where the engine said 1055; and WHERE names the 'where clause' where the projection names the 'field list'. The walk is built on subexpressions so an unknown AST variant fails open.

Review caught the fix doing the thing it was written to avoid: the qualifier was read off the rendered text, which keeps backticks, so SELECT `A`.* FROM A refused a statement MySQL answers -- a quiet wrong answer turned into a refusal. It now reads the identifiers, as using_condition already did. The corpus and tests had only ever spelled qualifiers unquoted, which is why nothing bit.

EXISTS then needed MySQL's own transformation rather than a scope rule. A qualified * resolves against the query's own FROM only -- 1 IN (SELECT A.* FROM B) is 1051 -- but EXISTS discards its select list before expanding any wildcard, so EXISTS (SELECT nope.* FROM B) answers. A HAVING keeps the list alive and restores the refusal, and a named column inside EXISTS is still resolved. A UNION of mismatched arities gives 1222, which is what proves the discarded item is replaced by one column rather than none.

Sixteen legal shapes confirmed still working, each prepared and run: four correlated-subquery forms, CTE by name, derived table by alias, self-join, JOIN USING, output alias in ORDER BY, db-qualified table, and @@GLOBAL.version -- which parses as a compound identifier, the same shape being refused.

One divergence left and asserted with its reason: EXISTS (SELECT nope.* FROM B HAVING COUNT(*) > 0) is 1235 here and 1051 on 8.3, because a qualified wildcard in a grouped query is unimplemented. Both refuse. It will re-surface when grouped wildcards land -- see #0100.

Filed rather than half-fixed: #0098 database qualifier ignored, #0099 wrong clause name in 1054, #0100 the remaining clauses unresolved at prepare, #0101 no-FROM is 1109 not 1054, #0102 DML over an empty table, #0103 HAVING without an aggregate, #0104 scalar subquery typed VAR_STRING.

18 mutations, all bite, including one that reintroduces the backtick bug. Two were inert and fixed by strengthening the test. Corpus 1044 -> 1160, baseline empty.
