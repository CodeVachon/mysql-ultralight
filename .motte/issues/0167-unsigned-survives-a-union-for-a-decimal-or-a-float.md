---
id: 167
title: UNSIGNED survives a UNION for a decimal or a float
state: Todo
parent: 9
labels: [compat]
created: 2026-09-03T09:05:53Z
updated: 2026-09-03T14:03:11Z
---

## Description

Found while implementing #0105, and reported rather than half-fixed.

MySQL drops the UNSIGNED flag through a UNION or a derived table for a DECIMAL UNSIGNED and a FLOAT UNSIGNED, keeping it only for an integer and a YEAR. This engine keeps it for all of them.

#0105 guarded the union merge with is_numeric rather than is_integer on purpose: the loop immediately below reads the same field to decide whether to untag the VALUES, so narrowing it there moves arithmetic as well as description, which is a wider change than that issue's subject.

Recorded in the code at that spot. Worth measuring the value half before changing the guard -- establish what a DECIMAL UNSIGNED holding a value above i64::MAX does through a UNION on 8.3, since that is where description and value can disagree.

## Notes

### 2026-09-03T14:03:11Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed, and the split in the title is exact: UNION of a DECIMAL UNSIGNED and of a FLOAT UNSIGNED keep UNSIGNED here (Flags UNSIGNED NUM) where 8.3 drops it (Flags NUM). The INT UNSIGNED case agrees on both.
