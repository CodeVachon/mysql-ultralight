---
id: 231
title: Named windows are unsupported
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-04T04:21:03Z
updated: 2026-09-04T04:21:03Z
---

## Description

Found while implementing #0100.

    SELECT ROW_NUMBER() OVER w FROM t WINDOW w AS (PARTITION BY zz.q)
    mysql   resolves it -- 1054 'window partition by' for the bad name
    engine  1235 does not yet support named windows

A clear refusal, so it is within the house rule, and the target application's 15 window functions all spell their OVER clause inline. Recording it because #0100 taught the engine to resolve an inline OVER spec's PARTITION BY and ORDER BY at prepare, and a named window would need the same treatment once it parses.
