---
id: 116
title: SIGN is unsupported
state: Done
parent: 9
labels: [executor]
created: 2026-08-25T23:13:29Z
updated: 2026-09-03T02:58:12Z
---

## Description

`SELECT SIGN(1.50)` is 1235 `mysql-ultralight does not yet support function \`sign()\``. MySQL answers -1, 0 or 1 as a LONGLONG.

A clear refusal rather than a wrong answer, so it is a gap and not a bug. Found while sweeping the functions that touch a decimal for #0070 — the rest of that sweep (ABS, ROUND, TRUNCATE, CEILING, FLOOR, COALESCE, IFNULL, IF, MIN, MAX, SUM, MOD) either agreed already or was fixed there.

## Notes

### 2026-09-03T02:58:12Z — Christopher Vachon (user)

Done in 2000290, in an isolated worktree. Much larger than the issue implied.

SIGN is a LONGLONG, length 21, decimals 0, binary charset, flags BINARY NUM, NOT_NULL exactly when its argument is, and never UNSIGNED even over an unsigned argument -- not ABS's shape, which stays in the argument's type.

A decimal is read exactly rather than through a double. That is invisible in a SQL answer, and the mutation for it is inert and says so: every decimal MySQL can hold (65 digits, 30 places) keeps its sign through an f64, so the exact path cannot change an answer. It is asserted at decimal.rs's own level, where a subnormal separates the two readings. The pair worth knowing: SIGN(0.1+0.2-0.3) is 0 and SIGN(0.1e0+0.2e0-0.3e0) is 1, because the decimal sum is exactly zero and the double is 5.55e-17.

Also measured: SIGN('abc') is 0 with warning 1292, not NULL; a temporal signs its packed digits; an ENUM signs its ordinal; -0.0 is 0 in both the decimal and double forms, which is why float_sign exists rather than f64::signum.

The wrong-argument-count case was the larger half. 8.3 answers 1582 from the PARSER, so it precedes 1146, 1054 and 1210 and fails a prepare, and the message echoes the name as the writer spelled it. Putting that refusal where the other resolve-time refusals live found two more places nothing was resolved at all: INSERT ... VALUES wrote a value where 8.3 raises, and a generated column, DEFAULT or CHECK was accepted at CREATE TABLE -- leaving the catalog and the append-only state image holding a table MySQL could not create. Both also close the malformed-escape analogue at those points, which 8.3 refuses too.

Review found the walk missed a call's own ORDER BY and a window's PARTITION BY/ORDER BY, a hole the escape refusal shared; both now go through one function_carried.

One ordering gap stays, documented: 8.3 raises 1582 before it looks a table up, and here a missing table still wins. Both refuse, and within a single expression the count wins as it does there.

Review also found a pre-existing wrong answer that is not this issue and is bigger than it: SELECT 1 WHERE 0 returns a row, because the FROM-less path never consults its WHERE clause -- filed as #0158.

Left measured but unencoded: a decimal literal with 330 places is truncated by 8.3 to 72 places so its SIGN is 0 there, where this engine keeps every place and answers 1. That is the literal's width, pre-existing.

25 mutation invocations. Corpus 2596 -> 2660, baseline empty, both gates verified independently on the merged tree.
