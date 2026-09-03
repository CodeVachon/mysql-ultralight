---
id: 143
title: Binariness does not survive an aggregate
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-08-31T20:28:22Z
updated: 2026-09-03T14:20:29Z
---

## Description

Found while implementing #0130.

    SELECT MAX(UNHEX(v))   mysql cs=63 flags=[BINARY]   engine text

#0130 made UNHEX and FROM_BASE64 the first computed columns this engine describes as bytes, and an aggregate over one loses that. So the binariness is right at the function and wrong one level up.

#0128's class -- a charset is a property of the expression in MySQL and only of the Value here -- seen through an aggregate rather than through a string function. #0130's measurement localised #0128 to case-folding and the column-charset comparison; this is the third place the same root cause shows.

Worth checking MIN, GROUP_CONCAT and the window forms in the same pass, and whether a derived table or CTE over the aggregate loses it again on top.

## Notes

### 2026-09-03T14:04:20Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed exactly, but ONLY visible in the metadata: MAX(UNHEX(v)) has the same VALUE on both sides, so a probe comparing rows alone falsely agrees. With --column-type-info: 8.3 reports Collation binary (63) and Flags BINARY; the engine reports utf8mb4_0900_ai_ci (255) and no flags.
