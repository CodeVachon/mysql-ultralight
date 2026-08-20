---
id: 45
title: A JSON column compares as text, not as a document
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T15:26:03Z
updated: 2026-08-20T15:26:03Z
---

## Description

Found while fixing #0037. The engine compares a JSON value by its rendered text, where MySQL compares by JSON semantics.

MySQL: SELECT doc = '{"a": 2, "b": 1}' FROM D is 0 even when the text is exactly what SELECT doc prints, because the string operand is read as a JSON *string* scalar rather than parsed, and an object never equals a string. So a JSON column compared against any string literal is false.

The engine answers by comparing text, so it is now accidentally right for the common spellings - the normalised form rarely matches a hand-written literal - and wrong whenever it does match: doc = '{"a": 1}' spelled with MySQL's own spacing is 1 here and 0 there.

The same applies to ordering and to DISTINCT over a JSON column, which use collation_key and so also compare text.

Doing this properly means comparing two Value::Json operands as documents and treating a JSON-to-string comparison as MySQL does. The tag added in #0033 is what makes it possible.
