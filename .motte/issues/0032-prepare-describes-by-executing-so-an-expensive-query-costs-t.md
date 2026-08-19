---
id: 32
title: Prepare describes by executing, so an expensive query costs twice
state: Done
parent: 6
labels: [executor]
created: 2026-08-19T02:15:44Z
updated: 2026-08-19T21:12:57Z
---

## Description

COM_STMT_PREPARE has to declare the result columns, because clients cache that metadata and lose every column name without it. `Engine::describe` establishes the shape by running the statement, which means an expensive prepared query is paid for twice: a 20k-row self-join measured 29s to describe and 29s again to execute.

It does not bite on test-sized data, and correctness came first: the declared types must match what execute sends, so describing against a rows-cleared catalog is not safe — computed column types are inferred from the first row.

Worth revisiting with a real planner, or a schema-only relation that carries column types without materialising rows.

Two smaller things found in the same review and deliberately left:
- Prepared column metadata goes stale after DDL. Execute always sends the real definitions, so only a client that caches prepare metadata sees it. Real MySQL raises 1615 'prepared statement needs to be re-prepared'; matching that needs a cheap describe first.
- `SELECT ?` describes its column as `NULL` rather than `?`, because parameters are bound to NULL for the probe. Every other shape describes correctly.

## Notes

### 2026-08-19T21:12:57Z — Christopher Vachon (user)

Landed. Describe now runs in a shaping mode: stored tables are read for their columns and not their rows. A thread-local carries it, because it has to reach through every recursion a FROM clause can take - views, derived tables, CTEs, scalar subqueries - and threading a flag through all of them would put the same argument in twenty signatures that do not care. A Drop guard restores it, because the flag left set is the worst state available: every later query on that thread would read stored tables as empty and answer successfully with no rows.

Measured: describing the issue's self-join went 2002ms to 0ms. On a 40-column table, 267us to 8us - the second factor came from removing a double clone, where relation_of cloned the whole Table just to rename it for an alias and from_table then cloned its columns again. Relation::from_table_named takes the display name instead, which helps execution too. information_schema is built empty rather than built and discarded, via info_schema::shape asking for the real thing against an empty catalog so there is no second list of column names to drift.

The safety net is crates/engine/tests/describe.rs: 45 query shapes described and executed, held to each other on name, type and nullability, with an assertion that every shape actually ran so a future regression cannot quietly shrink the coverage. Verified discriminating - making one column type row-dependent produces 19 mismatches.

Also fixed, from the review:
- Shaping broke describe for lateral JSON_TABLE. With no left rows the loop never runs and the fallback evaluated the correlated document with no row in scope, so every prepare over FROM t, JSON_TABLE(t.col, ...) declared zero columns. The fallback now builds a row of NULLs over the left schema. The underlying bug was pre-existing and is fixed with it: SELECT jt.n FROM E, JSON_TABLE(E.j, ...) on an empty table E used to fail at execution with 1054.
- describe_matches_execute_with_parameters asserted nothing - it only printed. It now asserts, including the one shape that legitimately differs.
- One of the 43 shapes was a syntax error and was being skipped silently, and it was the only one reaching values_result, the last place a column type is still taken from a value.

Stale metadata after DDL is done too, since this issue named the cheap describe as its blocker: COM_STMT_EXECUTE re-describes and answers 1615 when the cached metadata has changed. Statements that declared no columns skip the check, which is every write - nothing was cached to go stale, and binding a wide INSERT's placeholders only to throw the parse away cost 8.4 percent of the insert. The check costs about 3 percent of a prepared read (8us against 275us).

Still open, and deliberately:
- SELECT ? names its column NULL rather than ?, because the value is substituted before the statement is parsed. The declared type is right - MySQL also says VAR_STRING at prepare time - and only an unaliased bare placeholder is affected. Now asserted in the describe test rather than only described in prose.
- A schema-version counter would make the 1615 check free rather than 3 percent. Not done: it is a second source of truth about when the schema changed, and Prepared is cloned out of the connection's map on execute, so a stamp could not be updated in place without a wider refactor. Worth revisiting if prepared reads ever become the bottleneck.

604 migrations still apply, db pull still gives 209 models and 66 enums, conformance unchanged at 99.9 percent.
