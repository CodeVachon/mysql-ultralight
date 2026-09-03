---
id: 190
title: Temporal and conditional expressions keep the wrong width or type
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T12:52:56Z
updated: 2026-09-03T14:20:34Z
---

## Description

Found while implementing #0110. Two related shapes, both measured on 8.3:

    tm + 0  over a TIME     mysql int      engine bigint
    dd + 0, ts + 0          both bigint, agreeing

A TIME prints ten characters but computes in seven, so its arithmetic narrows where a DATE's and a TIMESTAMP's do not.

And the conditional family keeps the branch's narrow type rather than widening to int, over a TINYINT/SMALLINT:

    CASE WHEN 1 THEN ti END   mysql int    engine tinyint(1)
    COALESCE(ti, si)          mysql int    engine smallint
    GREATEST(ti, si)          mysql int    engine smallint

Note the engine is *narrower* than MySQL here, the opposite direction from #0110. The tinyint(1) case matters most: a client that reads tinyint(1) as a boolean sees one here and an integer against a real server.
