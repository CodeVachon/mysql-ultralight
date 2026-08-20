---
id: 43
title: The arrow operators render with spaces in a column name
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T14:21:40Z
updated: 2026-08-20T20:44:40Z
---

## Description

Found by the differential harness. A column named by doc->>'$.b' is called "doc->>'$.b'" by MySQL and "doc ->> '$.b'" here, because the name comes from rendering the parsed expression rather than the text as written.

Cosmetic, but a column name is what a client keys a row by, so a test reading the column by name gets nothing.

## Notes

### 2026-08-20T20:44:40Z — Christopher Vachon (user)

Wider than the arrows: MySQL names an unaliased column with its source text character for character, so no rendering of the tree can be right — 1!=2 and 1 <> 2 are one NotEq. The name now comes from the source, recovered by span with a round-trip guard. Also fixed two regressions the review found in #0042's DIV re-association, and the pre-existing arrow precedence bug behind them: sqlparser gives -> nearly the loosest precedence, so doc->'$.b' = 6 parsed as doc -> ('$.b' = 6). Split out #0056 (XOR), #0057 (EXTRACT), #0058 (describe does not resolve qualifiers).
