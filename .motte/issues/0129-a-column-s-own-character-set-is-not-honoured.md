---
id: 129
title: A column's own character set is not honoured
state: Todo
parent: 9
labels: [compat, app-priority-1]
created: 2026-08-26T11:18:03Z
updated: 2026-09-03T14:21:05Z
---

## Description

Found while implementing #0074.

MySQL applies the column's charset ctype table, not a codepoint range, so a VARCHAR CHARACTER SET latin1 disagrees with utf8mb4 about which characters are letters:

    SOUNDEX over a latin1 column holding '×abc'   mysql A120   engine ×120
    the same for '÷' (0xF7)

Both are punctuation in latin1 and pass the engine's codepoint >= 0xC0 test.

Two characters wide as measured, but the cause is general: the engine has no per-column character sets at all, so every text column behaves as utf8mb4. Anything charset-sensitive -- collation, LENGTH against CHAR_LENGTH, UPPER, comparison -- inherits it wherever a column is not utf8mb4.

Worth scoping before taking: implementing per-column charsets properly is large, and the target schema may use only utf8mb4, in which case recording the limit is the honest outcome rather than building it.

## Notes

### 2026-09-03T14:04:20Z — Christopher Vachon (user)

VALIDATED 2026-09-03 against mysql:8.3, same client asking both servers. Confirmed exactly. A latin1 column holding 0xD7 ('x' multiply sign) and 0xF7 ('divide') gives A120 on 8.3 for both; the engine returns the raw byte followed by 120 for both.

### 2026-09-03T14:21:05Z — Christopher Vachon (user)

APP PRIORITY 1 for application-mono-repo -- as the mechanism behind #0152 and #0134 rather than for its own SOUNDEX example.

The target declares CHARACTER SET on 288 sites and COLLATE on 1537, every one utf8mb4 / utf8mb4_unicode_ci. The engine accepts the DDL (measured: CREATE TABLE with that clause agrees) but does not honour the collation in comparison, LIKE or ORDER BY. So the declaration is stored and ignored, which is what makes #0152 and #0134 wrong-row bugs rather than curiosities.

Note the app uses no BINARY, VARBINARY or BLOB column and no non-utf8mb4 charset, so the latin1/binary examples in the description are not the pressing part -- the pressing part is that utf8mb4_unicode_ci is not applied.
