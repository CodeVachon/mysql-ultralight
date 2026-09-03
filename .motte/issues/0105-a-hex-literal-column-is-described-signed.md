---
id: 105
title: A hex literal column is described signed
state: In Progress
parent: 9
labels: [compat]
created: 2026-08-25T16:39:34Z
updated: 2026-09-03T08:26:12Z
---

## Description

Found by the new naming corpus group in #0059, and unrelated to naming -- that change touches only render_expr.

    SELECT X'41'   mysql UNSIGNED   engine signed
    SELECT 0x41    mysql UNSIGNED   engine signed

Four columns each across the text and binary protocols. The two corpus statements were narrowed rather than blessed, with the finding recorded in a comment at that spot in cases.rs, so the baseline stays empty.

Note 3b6a342 gave the engine Value::Unsigned and the machinery to describe a column unsigned, so this is now a matter of typing the literal rather than a missing mechanism.
