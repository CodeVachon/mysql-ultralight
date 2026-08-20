---
id: 36
title: String functions answer '' where MySQL answers NULL
state: Done
parent: 9
labels: [compat, executor]
created: 2026-08-20T14:21:10Z
updated: 2026-08-20T15:12:38Z
---

## Description

Found by the differential harness. UPPER, LOWER, TRIM, SUBSTRING, REPLACE, LEFT and RIGHT return an empty string for a NULL argument, and LENGTH returns 0. MySQL returns NULL for all of them.

This is the worst class the harness found, because it is a wrong answer rather than wrong metadata: WHERE UPPER(t) = '' matches rows here that MySQL would not return, and a test asserting NULL gets an empty string instead.

Evidence: SELECT CONCAT(t,'!'), UPPER(t), LOWER(t), LENGTH(t) FROM S over a row where t IS NULL gives NULL, NULL, NULL, NULL from MySQL and NULL, '', '', '0' here. CONCAT alone propagates correctly, which is why this went unnoticed.

## Notes

### 2026-08-20T15:12:38Z — Christopher Vachon (user)

Fixed. The issue named eight functions; probing every function the engine implements against real MySQL found twenty-six. So the rule was fixed rather than the functions: MySQL returns NULL from a scalar function if any argument is NULL, and functions::call now applies that up front, with a NULL_TOLERANT list for the exceptions.

Keeping it as a list of exceptions is the point. The functions that got this wrong were the ones that never considered NULL, so the safe answer has to be the one you get by not thinking about it - a function added tomorrow propagates correctly without its author knowing the rule exists.

One rule fixed 24 of the 26. The other two are CEIL and FLOOR, which turned out not to work at all: sqlparser gives them dedicated AST nodes the engine never handles, so their arms in functions.rs are unreachable from SQL. Filed as #0044 rather than folded in.

A property fell out for free: the rule keys on having a NULL argument rather than on the name, so UNIX_TIMESTAMP() still means now while UNIX_TIMESTAMP(NULL) is NULL. That one used to answer with the current time.

Three bugs in the exceptions themselves, two found by probing them and one by review:

- CONCAT_WS exempted wholesale answered 'ab' for CONCAT_WS(NULL,'a','b'). Only the values are skipped; a NULL separator makes the whole call NULL.
- JSON_SET and JSON_ARRAY_APPEND exempted wholesale sent a NULL *path* to the path parser and failed with 3143 where MySQL answers NULL. A NULL value is stored as a JSON null; a NULL path is not.
- Worst of the three, from review: the rule ran before name dispatch, so SELECT NOT_A_FUNCTION(NULL) answered NULL instead of erroring. A query over a column that happened to be all NULL would succeed until the day one row had a value in it - the 'unsupported means a clear error, never a wrong answer' convention broken by the fix meant to uphold it. There is now a KNOWN list of the 96 dispatched names that the rule checks first, held to the match by a test; drift makes a function unsupported rather than wrong, which is the direction to drift in.

Left deliberately: the per-function is_null checks are now redundant for the NULL case but several guard a *missing* optional argument, which the general rule does not cover. Verified JSON_LENGTH('[1,2]'), SUBSTRING('hello',2), ROUND(1.55) and the rest still match MySQL.

The harness reports the three original row divergences as fixed, and the new sixteen-case group adds no row divergences of its own - only the already-known type-metadata class. 604 migrations still apply, db pull byte-identical.
