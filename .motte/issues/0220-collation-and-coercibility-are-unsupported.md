---
id: 220
title: COLLATION() and COERCIBILITY() are unsupported
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-04T02:36:38Z
updated: 2026-09-04T02:36:38Z
---

## Description

Found while implementing #0148/#0154, measured on 8.3 under utf8mb4_0900_ai_ci.

    SELECT COLLATION('x'), COERCIBILITY('x')
    mysql   utf8mb4_0900_ai_ci, 4
    engine  1235 does not yet support function 'collation()'

A clear refusal rather than a wrong answer, so it is not urgent -- but it makes every collation claim in this area unmeasurable from SQL alone, which is why it is worth having. The engine now models collation and coercibility internally (crates/engine/src/collation.rs: Collation, Coercibility with SysConst=3 and Numeric=5), so both functions are readers of state that already exists rather than new mechanism.

Also unsupported, found incidentally: ELT() and REPEAT().
