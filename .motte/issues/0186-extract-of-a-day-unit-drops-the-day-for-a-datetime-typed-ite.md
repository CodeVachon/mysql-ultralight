---
id: 186
title: EXTRACT of a DAY unit drops the day for a datetime-typed item
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-09-03T12:52:35Z
updated: 2026-09-03T14:20:33Z
---

## Description

Found while implementing #0094, measured on 8.3 with dt = '2019-03-04 05:06:07.891234'.

MySQL keeps the day for some shapes and zeroes it for others, and the split is not about the value:

    keeps it (4050607):  dt, d, ts, s columns; through a view or derived table;
                         (dt); the literal '2019-03-04 05:06:07';
                         dt + INTERVAL 0 SECOND; DATE_ADD(...);
                         CASE WHEN 1 THEN dt ELSE dt END; MAX(s);
                         (SELECT MAX(dt) FROM ...)
    drops it (50607):    MAX(dt), MAX(ts), CAST(dt AS DATETIME(6)),
                         COALESCE(dt), GREATEST(dt,dt), TIMESTAMP(dt),
                         TIMESTAMP '2019-03-04 05:06:07.891234'
                         and MAX(d) over a DATE is 0

This is Item::get_time_from_datetime(), which zeroes the date, versus reading through text. The engine implements the column/literal behaviour, which is the majority and the one people write.

The engine's Value::Temporal-vs-text model has no counterpart for the distinction, so this is not a small fix -- it needs a notion of which items are datetime-typed in MySQL's sense. The affected shapes are recorded in a comment beside the clock flag in expr.rs, and no corpus case asks a diverging shape.
