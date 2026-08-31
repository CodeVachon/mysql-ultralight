---
id: 130
title: HEX is unsupported
state: In Progress
parent: 9
labels: [executor]
created: 2026-08-26T11:18:03Z
updated: 2026-08-31T17:26:33Z
---

## Description

SELECT HEX('abc') raises 1235. MySQL answers 616263.

Found while implementing #0074, where it was the direct obstacle: every byte-level measurement in that issue had to be run against MySQL alone, because no differential case could express HEX(SOUNDEX(...)) -- the one function that makes a byte-level answer visible at all.

That is the argument for doing it beyond the function itself: without HEX, no corpus case can check what any function returns at the byte level, so a whole class of divergence is invisible to the harness. #0074's binary-charset bug was found by reading MySQL's source and probing by hand, not by the corpus, and HEX is why.

Check UNHEX, TO_BASE64/FROM_BASE64 and BIN in the same pass, and what each returns for a binary argument as against a text one.
