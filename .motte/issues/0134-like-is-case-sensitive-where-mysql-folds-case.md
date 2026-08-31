---
id: 134
title: LIKE is case-sensitive where MySQL folds case
state: Todo
parent: 9
labels: [executor]
created: 2026-08-31T13:34:50Z
updated: 2026-08-31T13:34:50Z
---

## Description

Found while implementing #0093.

    SELECT 'ABC' LIKE 'a%'   mysql 1   engine 0
    SELECT 'A' = 'a'         mysql 1   engine 1

The engine is inconsistent with itself: equality folds case under the default collation and LIKE does not. MySQL's utf8mb4_0900_ai_ci folds for both.

Not fixable by folding LIKE unconditionally -- the engine answers BINARY 'A' LIKE 'a' correctly today (0), and a blanket fold would break it. A real fix needs the collation to reach the comparison, which is #0129's subject.

Every case in #0093's new corpus group is same-case ASCII on purpose, with that reason written above the group, so this gap is not hidden by the empty baseline -- it is simply not exercised.

Likely to matter for the target schema: a case-insensitive LIKE is what a client gets from MySQL by default, so any filter written against real data will disagree here.
