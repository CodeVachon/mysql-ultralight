---
id: 79
title: A bitwise operand reads a temporal or decimal by its leading digits
state: Todo
parent: 9
labels: [executor]
created: 2026-08-24T17:36:11Z
updated: 2026-08-24T17:36:11Z
---

## Description

Regression introduced by #0052 in 846c925, found by review during #0053.

bit_operand runs integer_prefix over a Value::Text, so any value the engine stores as text is read by its leading digits rather than converted the way MySQL converts it:

    CAST('2026-01-02' AS DATE) | 0            mysql 20260102   engine 2026
    CAST('3.7' AS DECIMAL(10,1)) | 0          mysql 4          engine 3

The decimal case is the plainer bug: #0052 measured and encoded that a number operand rounds away from zero, and a decimal that reached the operator as text never reaches that rule. The temporal case is the same shape as #0054 -- the value has lost its declared type before the operator sees it -- and will likely fall out of whatever #0054 does.

A wrong answer rather than a refusal, which the project rule forbids. Until the type reaches the operator, the honest behaviour is 1235.
