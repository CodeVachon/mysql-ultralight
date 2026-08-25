---
id: 110
title: A view column is materialised wider than MySQL materialises it
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T19:32:32Z
updated: 2026-08-25T19:32:32Z
---

## Description

Found by the corpus while implementing #0059, and about a view's shape rather than its names.

    CREATE VIEW LM AS SELECT 1 MOD 3;  SELECT * FROM LM
    mysql   INT
    engine  BIGINT

The corpus case was removed rather than blessed, with the reason recorded in cases.rs, so the baseline stays empty. The same view is still covered by name in the unit test.

Related in kind to #0038, which narrowed the integer type codes so an expression is described the way MySQL describes it. This is the materialised column a view creates from that expression, which is a second place the width is decided and evidently a different rule.

Worth measuring several expressions rather than fixing 1 MOD 3 alone: whether the width follows the operands, the operator, or the range the result can occupy.
