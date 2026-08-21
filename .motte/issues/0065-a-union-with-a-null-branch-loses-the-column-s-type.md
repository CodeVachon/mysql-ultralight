---
id: 65
title: A UNION with a NULL branch loses the column's type
state: Done
parent: 9
labels: [compat]
created: 2026-08-21T13:08:59Z
updated: 2026-08-21T17:53:05Z
---

## Description

SELECT id FROM N UNION SELECT NULL ORDER BY 1
      type of column 0   mysql MYSQL_TYPE_LONG   engine MYSQL_TYPE_VAR_STRING
      row 1              mysql Int(1)            engine "1"

The set-operation merge in crates/engine/src/exec.rs widens the two branches and falls back to a string where there is no wider number. A NULL branch has no type to widen against, so it takes that fallback — but NULL is not a competing type, it is the absence of one, and MySQL keeps the INT and only relaxes the nullability.

The row difference is the same cause seen from the other side: declared VAR_STRING, the value goes out as text rather than as an integer, so a binary client reads "1" where MySQL sends Int(1).

Three of the 75 baseline lines.

## Notes

### 2026-08-21T17:37:13Z — Christopher Vachon (user)

All 3 lines fixed; baseline 7 to 4. Measuring the cases the issue did not name turned up a second bug, mine from #0064: column.unsigned &= right.unsigned ran before the comparison that checks whether the signs differ, so a mixed-sign union matched itself and stayed a LONG where MySQL widens to LONGLONG. The flags are merged after the comparison now.

### 2026-08-21T17:53:05Z — Christopher Vachon (user)

Review found the same defect class one width apart: widen bumped the rank only when the two integers were the *same* size, so INT UNSIGNED UNION SMALLINT came back a signed LONG and handed a binary client -1 for 4294967295. The rule is max(signed rank, unsigned rank + 1), falling to DECIMAL past BIGINT — which is how MySQL answers BIGINT UNSIGNED UNION INT. Also: the corpus had only the branch orders that never failed, so both reversed statements are in it now.
