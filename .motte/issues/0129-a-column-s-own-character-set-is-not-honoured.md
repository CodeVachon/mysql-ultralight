---
id: 129
title: A column's own character set is not honoured
state: Todo
parent: 9
labels: [compat]
created: 2026-08-26T11:18:03Z
updated: 2026-09-02T14:02:43Z
---

## Description

Found while implementing #0074.

MySQL applies the column's charset ctype table, not a codepoint range, so a VARCHAR CHARACTER SET latin1 disagrees with utf8mb4 about which characters are letters:

    SOUNDEX over a latin1 column holding '×abc'   mysql A120   engine ×120
    the same for '÷' (0xF7)

Both are punctuation in latin1 and pass the engine's codepoint >= 0xC0 test.

Two characters wide as measured, but the cause is general: the engine has no per-column character sets at all, so every text column behaves as utf8mb4. Anything charset-sensitive -- collation, LENGTH against CHAR_LENGTH, UPPER, comparison -- inherits it wherever a column is not utf8mb4.

Worth scoping before taking: implementing per-column charsets properly is large, and the target schema may use only utf8mb4, in which case recording the limit is the honest outcome rather than building it.
