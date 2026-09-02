---
id: 152
title: The engine's default collation is not utf8mb4_0900_ai_ci
state: Todo
parent: 9
labels: [executor]
created: 2026-09-02T14:02:23Z
updated: 2026-09-02T14:02:23Z
---

## Description

Found by the charset-cluster scoping pass, and untracked until now despite being the largest real-world divergence in that cluster.

MySQL's default collation is accent-insensitive and this engine's is not:

    SELECT 'cafe' = 'café'      mysql 1   engine 0
    SELECT 'CAFÉ' = 'cafe'      mysql 1   engine 0
    SELECT 'ss' = 'ß'           mysql 1   engine 0
    SELECT 'café' LIKE 'cafe'   mysql 1   engine 0
    ORDER BY over a, á, B       mysql a, á, B   engine a, B, á

The engine folds case and then compares codepoints, so accents sort after every ASCII letter and never compare equal.

Why it matters more than #0129: 305 columns in the conformance target are declared utf8mb4_unicode_ci, where this is live for every =, LIKE, GROUP BY, DISTINCT and unique key. #0129's per-column charsets, by contrast, are used by zero columns there -- all 78 CHARACTER SET clauses are utf8mb4.

crates/engine/src/collation.rs documents the accent half as knowingly open; this issue is that record made trackable.

Scoping note for whoever takes it: this is the one part of collation-aware comparison that needs collation DATA rather than dispatch. Deciding which collation applies is cheap and closes most of the cluster; comparing correctly under it needs weight tables (DUCET, or MySQL's own). Decide which of the two is in scope before starting -- see the staged plan recorded on #0129.
