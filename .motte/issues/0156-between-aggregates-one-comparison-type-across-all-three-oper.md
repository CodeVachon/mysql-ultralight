---
id: 156
title: BETWEEN aggregates one comparison type across all three operands
state: Todo
parent: 9
labels: [executor]
created: 2026-09-03T02:20:16Z
updated: 2026-09-03T02:20:16Z
---

## Description

Found by review during #0092, and a different kind of gap from that issue's three-valuedness.

    SELECT '2' BETWEEN 1 AND '10'     mysql 1   engine 0
    SELECT '10' BETWEEN '9' AND 100   mysql 1   engine 0

MySQL picks ONE comparison type by aggregating all three operands, so an integer bound makes the subject and both bounds numeric. This engine decomposes BETWEEN into two pairwise comparisons, and the pair '2' <= '10' collates -- which is 0 on both servers, correctly, for that pair alone.

So the decomposition a >= lo AND a <= hi only holds where the three operands already agree on type. #0092 corrected the arm's comment to say so and recorded the measurement there.

Deliberately not half-fixed: getting it right is a rule per type family -- temporals, decimals, ENUM, JSON each aggregate differently -- and a partial version would make the gap depend on which types were mixed. No corpus case, because it would need a baseline line, and a baseline line without an issue is what the README forbids.

Related: #0091 (BETWEEN's high operand does not take a predicate) got the same treatment in postfix_operands. Both are BETWEEN gaps recorded rather than guessed at.
