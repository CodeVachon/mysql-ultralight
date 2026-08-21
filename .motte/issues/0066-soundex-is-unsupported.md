---
id: 66
title: SOUNDEX is unsupported
state: Done
parent: 9
labels: [executor]
created: 2026-08-21T13:08:59Z
updated: 2026-08-21T19:02:52Z
---

## Description

SELECT SOUNDEX(NULL) is refused with 1235 where MySQL answers one row. Two of the differential baseline's lines. The corpus reaches it through the NULL-propagation group rather than for its own sake, so the argument being NULL is incidental — the function is simply absent from crates/engine/src/functions.rs. MySQL's algorithm is the standard Soundex with its own handling of leading vowels and repeated codes, and is worth measuring rather than recalling.

## Notes

### 2026-08-21T18:43:39Z — Christopher Vachon (user)

Implemented from measurement. MySQL's Soundex differs from most descriptions in three places, each of which a plausible implementation gets wrong: a vowel does not break a run of one code, so TYMCZAK is T520 and not T522; the first letter seeds that run, so PFISTER is P236 with the F swallowed by the P; and the result is not truncated to four, so ASHCRAFT is A2613. A letter outside ASCII counts as a letter and codes as zero. The baseline is now empty.

### 2026-08-21T19:02:52Z — Christopher Vachon (user)

Review found the letter test: MySQL's is a-z, A-Z or a byte at or above 0xC0, not a Unicode property — so Rust's is_alphabetic disagrees in both directions. Applied at the codepoint level here; exact agreement past ASCII is not reachable because MySQL runs it over bytes and emits a lone lead byte as the first character, which a Rust string cannot hold. #0074. Corpus gained the three rules that had lived only in unit-test expectations.
