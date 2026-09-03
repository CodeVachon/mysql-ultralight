---
id: 139
title: A binary string compares against text through the collation
state: In Progress
parent: 9
labels: [executor]
created: 2026-08-31T20:28:05Z
updated: 2026-09-03T01:51:05Z
---

## Description

Found while implementing #0130, and a live wrong answer.

    SELECT CAST('AB' AS BINARY) = 'ab'   mysql 0   engine 1
    SELECT X'4142' = 'ab'                mysql 0   engine 1
    a VARBINARY column against a string literal -- same

crates/engine/src/value.rs's Value::compare takes the byte-exact branch only when BOTH operands are Value::Bytes; a Bytes/Text pair falls through to collate(as_text, as_text), which is case-insensitive. MySQL makes the comparison binary if either side is.

This is why UNHEX('4142') = 'ab' could not become a differential case in #0130 -- it diverges today.

Adjacent to #0128 (a string function loses the binary charset) but distinct: there the binariness is lost on the way out of a function, here it is present on one side and ignored by the comparison. Whoever takes one should read the other.
