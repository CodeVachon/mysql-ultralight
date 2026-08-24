---
id: 83
title: A concatenation of two literals is typed nullable
state: Todo
parent: 9
labels: [compat]
created: 2026-08-24T18:02:26Z
updated: 2026-08-24T18:02:26Z
---

## Description

Found while building the ANSI differential group for #0053.

    SELECT 'a' || 'b'   under PIPES_AS_CONCAT   mysql NOT NULL   engine nullable

Pre-existing and never exercised, because no differential group ran under PIPES_AS_CONCAT before #0053 added one. Neither operand can be NULL, so neither can the result, and never_null already reasons about exactly this for other operators -- see #0063, which taught the describe path that a computed column that cannot be NULL is reported NOT NULL.

Kept out of the corpus for the same reason as the information_schema divergence found beside it: it would have cost the new group its empty baseline over a gap that is not that group's subject.
