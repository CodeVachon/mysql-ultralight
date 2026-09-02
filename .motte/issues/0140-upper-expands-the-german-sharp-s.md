---
id: 140
title: UPPER expands the German sharp s
state: In Progress
parent: 9
labels: [executor]
created: 2026-08-31T20:28:05Z
updated: 2026-09-02T14:02:43Z
---

## Description

Found through HEX(UPPER('straße')) while implementing #0130.

    SELECT UPPER('straße')   mysql STRAßE   engine STRASSE

Rust's str::to_uppercase performs the full Unicode uppercase mapping, which expands U+00DF to SS. MySQL's utf8mb4 case folding is a per-character table and leaves it alone.

A wrong answer, and a length change as well -- LENGTH and CHAR_LENGTH of the result differ, so it is visible beyond the characters themselves.

Worth checking LOWER in the same pass, and the other characters whose Unicode uppercase mapping is multi-character (U+FB00-U+FB06 ligatures, U+0149, U+01F0, the Greek iota-subscript forms). The fix is a per-character mapping rather than to_uppercase, which also matters for #0129 once per-column charsets exist.
