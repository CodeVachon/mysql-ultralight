---
id: 135
title: NO_BACKSLASH_ESCAPES is not modelled
state: Todo
parent: 9
labels: [compat, app-priority-3]
created: 2026-08-31T13:34:50Z
updated: 2026-09-03T14:20:29Z
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

## Notes

### 2026-09-03T14:04:20Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed. Under NO_BACKSLASH_ESCAPES, 8.3 reads the backslash literally -- LENGTH 5 -- where the engine still escapes it and gives 4.
