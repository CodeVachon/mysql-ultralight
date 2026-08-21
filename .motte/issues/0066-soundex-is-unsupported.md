---
id: 66
title: SOUNDEX is unsupported
state: Todo
parent: 9
labels: [executor]
created: 2026-08-21T13:08:59Z
updated: 2026-08-21T13:08:59Z
---

## Description

SELECT SOUNDEX(NULL) is refused with 1235 where MySQL answers one row. Two of the differential baseline's lines. The corpus reaches it through the NULL-propagation group rather than for its own sake, so the argument being NULL is incidental — the function is simply absent from crates/engine/src/functions.rs. MySQL's algorithm is the standard Soundex with its own handling of leading vowels and repeated codes, and is worth measuring rather than recalling.
