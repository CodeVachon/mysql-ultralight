---
id: 63
title: A computed column is reported nullable when it cannot be NULL
state: Done
parent: 9
labels: [compat]
created: 2026-08-21T13:08:38Z
updated: 2026-08-21T13:22:33Z
---

## Description

The largest remaining group in the differential baseline — 34 of its 75 lines, across four case groups — and every one is the same direction: MySQL says NOT NULL, this engine says nullable.

    SELECT COUNT(*), COUNT(v), COUNT(DISTINCT k) FROM G      columns 0, 1, 2
    SELECT AVG(t), MIN(t), MAX(t), COUNT(t) FROM Q           column 3
    SELECT COUNT(*), SUM(1), MAX(2)                          column 0
    SELECT k, COUNT(*) c FROM G GROUP BY k HAVING COUNT(*)>1 column 1
    SELECT id, ROW_NUMBER() OVER (ORDER BY id) FROM G        column 1
    SELECT id, LAG(id, 1, 0) OVER (ORDER BY id) FROM G       column 1
    SELECT COALESCE(NULL,1), IFNULL(NULL,1), IF(NULL,1,2), ISNULL(NULL)   all four
    SELECT COALESCE(t,'none'), IFNULL(n,0) FROM S            columns 0, 1
    SELECT t IS NULL FROM S                                  column 2
    SELECT UNIX_TIMESTAMP(NULL), UNIX_TIMESTAMP() > 0        column 1

There are several rules underneath, not one:

- COUNT never answers NULL, whatever it counts. Nor does ROW_NUMBER, nor a window function given a default.
- COALESCE, IFNULL and IF are NOT NULL when *some* branch is, which is the same all-branches reasoning `combined` already does for the type in crates/engine/src/exec.rs.
- A predicate — IS NULL, a comparison, LIKE — is 0 or 1 and never NULL.
- A no-argument function like UNIX_TIMESTAMP() has nothing to be NULL from.

Nullability is not cosmetic here: crates/engine/src/exec.rs already relies on it for the set-operation merge, where a branch that can produce NULL relaxes the other, and a describing client may map a NOT NULL column to a non-optional type.

## Notes

### 2026-08-21T13:22:33Z — Christopher Vachon (user)

All 34 lines fixed; baseline 75 to 41. Every rule measured rather than reasoned, and the split is not the obvious one: LENGTH('a') is NOT NULL where UPPER('a') is not, ABS(1) is where SQRT(4) is not, and MOD is nullable like the % it spells. Aggregates and window functions needed the rule applying on two more paths that build their own OutColumn. The UNSIGNED flag on ROW_NUMBER is all that remains on those statements, under #0064.
