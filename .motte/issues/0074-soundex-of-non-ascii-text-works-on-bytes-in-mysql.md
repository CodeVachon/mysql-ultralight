---
id: 74
title: SOUNDEX of non-ASCII text works on bytes in MySQL
state: Todo
parent: 9
labels: [compat]
created: 2026-08-21T18:55:46Z
updated: 2026-08-21T18:55:46Z
---

## Description

MySQL's SOUNDEX reads its input a byte at a time and emits the first byte it accepts verbatim, so the answer for multi-byte input is not text at all:

    SELECT HEX(SOUNDEX('€abc'))   E2313230   -- 0xE2, then "120"
    SELECT HEX(SOUNDEX('µabc'))   C2313230   -- 0xC2, then "120"
    SELECT HEX(SOUNDEX('🙂abc'))  F0313230

Every one of those leading bytes is the lead byte of a UTF-8 sequence with its continuation bytes dropped — MySQL's my_uni_isalpha is `a-z || A-Z || byte >= 0xC0`, which the lead byte passes and the continuations fail.

crates/engine/src/functions.rs applies that same 0xC0 threshold, but to whole codepoints, because a Rust String cannot hold a lone continuation byte. ASCII agrees exactly; anything above it agrees on the digits and differs on the first character, where this keeps `é` and MySQL sends 0xC3.

Matching it would mean SOUNDEX returning bytes rather than text — Value::Bytes rather than Value::Text — which is a change to what the function's type is for a case nothing is known to need. Recorded rather than done. Found by review during #0066.
