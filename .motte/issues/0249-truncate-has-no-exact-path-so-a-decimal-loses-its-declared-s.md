---
id: 249
title: TRUNCATE has no exact path, so a decimal loses its declared scale
state: In Progress
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T09:47:09Z
updated: 2026-09-04T10:04:43Z
---

## Description

Found while implementing #0223, which gave ROUND an exact path for decimals and left TRUNCATE on the double reading.

    TRUNCATE(<DECIMAL(10,3)> 2.500, 2)
    mysql   2.50
    engine  2.5

This is #0223's shape for a different function: the value diverges from the type the engine itself declares, and a decimal is routed through an f64 where exact digit arithmetic exists (crates/engine/src/decimal.rs, and ROUND's new round_exactly / decimal_scaled / signed_scaled).

Priority 2 because the target application declares 155 DECIMAL columns and TRUNCATE over money is exactly where the lost scale shows.
