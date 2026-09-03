---
id: 134
title: LIKE is case-sensitive where MySQL folds case
state: Done
parent: 9
labels: [executor, app-priority-1]
created: 2026-08-31T13:34:50Z
updated: 2026-09-03T16:32:22Z
---

## Description

Found while implementing #0093.

    SELECT 'ABC' LIKE 'a%'   mysql 1   engine 0
    SELECT 'A' = 'a'         mysql 1   engine 1

The engine is inconsistent with itself: equality folds case under the default collation and LIKE does not. MySQL's utf8mb4_0900_ai_ci folds for both.

Not fixable by folding LIKE unconditionally -- the engine answers BINARY 'A' LIKE 'a' correctly today (0), and a blanket fold would break it. A real fix needs the collation to reach the comparison, which is #0129's subject.

Every case in #0093's new corpus group is same-case ASCII on purpose, with that reason written above the group, so this gap is not hidden by the empty baseline -- it is simply not exercised.

Likely to matter for the target schema: a case-insensitive LIKE is what a client gets from MySQL by default, so any filter written against real data will disagree here.

## Notes

### 2026-09-03T14:01:18Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. 'ABC' LIKE 'a%' mysql 1 engine 0, while 'A'='a' is 1 on both -- the self-inconsistency the description names. Also seen: BINARY 'A' LIKE 'a' answers 0 on both but the engine returns it as a string where 8.3 returns a number.

### 2026-09-03T14:21:05Z — Christopher Vachon (user)

APP PRIORITY 1 for application-mono-repo. Measured against the real conformance surface on 2026-09-03.

The app runs ~137 LIKE-based searches -- 72 raw LIKE in apps/api/src and 65 Prisma contains/startsWith/endsWith filters, which Prisma compiles to LIKE. `mode: 'insensitive'` appears ZERO times, so every one of them depends on the column collation to fold case. Every text column is declared utf8mb4_unicode_ci (1537 COLLATE clauses, all the same one).

Measured under that exact declaration:
    rows: 'café', 'Cafe', 'Straße'
    WHERE name LIKE 'caf%'   mysql café + Cafe    engine café only

So a search box that finds two rows against MySQL finds one here. This is a wrong result set on the app's main read path, not a metadata difference.

### 2026-09-03T16:32:22Z — Christopher Vachon (user)

Landed in 44df38b as part of the collation mechanism, gate green. Verified against the exact probe that exposed it: rows café/Cafe/Straße under utf8mb4_unicode_ci, WHERE name LIKE 'caf%' now returns café AND Cafe, matching 8.3. LIKE folds per character but applies no expansions and no padding -- 'Straße' LIKE 'strasse' is 0 under every collation while = 'strasse' is 1 under two of them, so the two paths are separate by design. BINARY 'A' LIKE 'a' still answers 0.
