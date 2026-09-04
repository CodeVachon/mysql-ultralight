---
id: 154
title: A literal's case fold uses the wrong collation
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-02T17:25:27Z
updated: 2026-09-04T02:47:23Z
---

## Description

Found by the full-codepoint sweep in #0140.

The case-folding table belongs to the COLLATION, not the character set, and utf8mb4 has two on 8.3 that disagree on 616 characters upward and 608 downward:

    utf8mb4_0900_ai_ci   UPPER changes 1321 chars, folds above the BMP
    utf8mb4_general_ci   UPPER changes 706 chars, never folds above the BMP
    HEX(UPPER('ȿ'))      '' under 0900_ai_ci, C8BF under general_ci
    HEX(LOWER('ẞ'))      C39F under 0900_ai_ci, E1BA9E under general_ci

A column declared without COLLATE gets 0900_ai_ci; a literal takes the CONNECTION's collation, which mysql2 and the differential harness's client both negotiate as 45 = general_ci. So 8.3 answers SELECT UPPER('𐐨') and SELECT UPPER(col) differently for the same character.

#0140 encoded 0900_ai_ci, which is right for a column and wrong for a literal:

    SELECT UPPER('𐐨')     engine F0909080   8.3 F09090A8
    SELECT UPPER('aȿb')   engine 41E2B1BE   8.3 61C8BF62

The corpus asks those characters only through a column, with the reason recorded.

Closing this needs the engine to know a literal's collation is the connection's, which is #0129/#0152 territory -- the same missing mechanism. Note it also means the answer depends on how the client connected, so a corpus case must pin the connection collation or it will drift.

## Notes

### 2026-09-03T14:00:44Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3 with the connection collation pinned to utf8mb4_0900_ai_ci on BOTH sides (both servers report that value for @@collation_connection).

Under that connection, of the three examples only one diverges:

    HEX(UPPER('ȿ'))    mysql ''       engine C8BF     DIVERGES
    HEX(LOWER('ẞ'))    agrees
    HEX(UPPER('𐐨'))    agrees

Which is the point of the issue, sharpened: the engine *reports* utf8mb4_0900_ai_ci and *behaves* as general_ci for case folding. The description's framing -- that the client negotiates 45/general_ci -- is not the only way to see it; pin the connection to 0900_ai_ci and the engine still gives the general_ci answer, so this is the engine's fold table, not the client's negotiation.

Anyone measuring this must state the connection collation, or the measurement is unreproducible.

### 2026-09-04T02:47:23Z — Christopher Vachon (user)

Landed in a67339c, gate green. The casefold table DID need to become collation-aware, not just the comparison path -- and the answer is TWO tables, not four: derived by folding all 1,112,064 codepoints under a column of each collation, utf8mb4_0900_ai_ci changes 1321 up / 1304 down while general_ci, unicode_ci and bin change 706 / 696 and agree with each other on every codepoint in both directions.

The corpus/client problem this issue warned about was real and is now handled three ways: the engine honours the handshake charset; reset pins SET NAMES before every group; and check_identities compares @@collation_connection across both servers before the first reset -- the only moment the handshake value is visible -- and aborts rather than blaming the engine. The harness's driver sends 45 (general_ci) and never issues SET NAMES, confirmed in its source.

STILL OPEN, deliberately: the nineteen widening folds under a _0900_ connection (HEX(UPPER('ȿ')) is '' on 8.3 and C8BF here). The earlier author's measured argument for declining rather than truncating still stands and is now sharper -- the legacy table has none of the nineteen, so under the other three collations the engine is exactly right.
