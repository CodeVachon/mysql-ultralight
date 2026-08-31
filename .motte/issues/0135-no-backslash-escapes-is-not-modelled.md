---
id: 135
title: NO_BACKSLASH_ESCAPES is not modelled
state: Todo
parent: 9
labels: [compat]
created: 2026-08-31T13:34:50Z
updated: 2026-08-31T13:34:50Z
---

## Description

Found while implementing #0093.

Measured with the pattern bytes held constant, so the difference is the mode and not the literal:

    SELECT 'a%' LIKE 'a\%', HEX('a\%')
    default mode              mysql 1, 615C25   engine 1, 615C25
    NO_BACKSLASH_ESCAPES      mysql 0, 615C25   engine 1, 615C25

The mode is absent from SqlMode entirely and from the tokenizer, so a backslash is always an escape both in a string literal and as LIKE's default escape character.

#0093 deliberately did not half-model it: fixing LIKE's default without the tokenizer would trade one wrong answer for another, and the two have to move together. The measurement is recorded in like_escape's comment.

Note the sibling flags are all live -- see #0053, which applied REAL_AS_FLOAT and set the five flags ANSI actually sets -- so the machinery to carry this one already exists; what is missing is the tokenizer half.
