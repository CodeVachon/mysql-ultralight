---
id: 33
title: JSON values are text, so nested constructors double-encode
state: Done
parent: 6
labels: [executor, compat]
created: 2026-08-19T12:47:14Z
updated: 2026-08-19T21:50:14Z
---

## Description

JSON_OBJECT('a', JSON_ARRAY(1, 2)) yields {"a": "[1,2]"} where MySQL yields {"a": [1, 2]}.

The engine has no JSON-typed value: JSON is stored, compared and returned as text, which is how it arrives and leaves on the wire. By the time one JSON function's result reaches another it is an ordinary string, and json::from_value embeds it as one. Nothing distinguishes a document from a string that happens to look like one, so this cannot be detected and refused either - it is the one place the engine can be silently wrong rather than clearly unsupported.

The fix is a distinct Value::Json variant, which touches value.rs, the wire encoding, coercion, comparison, and the state image format (a new tag, so FORMAT_VERSION goes to 2).

Not urgent: nothing in the 604 migrations nests a JSON constructor. Worth doing before anyone relies on building documents from parts.

## Notes

### 2026-08-19T21:50:14Z — Christopher Vachon (user)

Landed. Value::Json(String) holds the serialised document but is distinguishable from text, which is the whole point - nothing about the string [1,2] says whether it was meant as a document or as a string, so the tag is the only thing that can tell them apart. JSON_OBJECT('a', JSON_ARRAY(1, 2)) now gives {"a": [1,2]}.

Everything that builds a document returns one, a JSON column coerces to one and validates on the way in (3140, with serde's own message and position), and CAST(... AS JSON) parses rather than relabels. A string stays a string: guessing by whether it parses would turn a column holding [9,9] into an array nobody asked for. The tag reaches the state image as value tag 5, appended rather than inserted, and is re-validated on load so a Value::Json always holds something that parses - which is what FORMAT_VERSION 2 is for.

Declared column types were deliberately left alone. A computed JSON expression stays VarString, as dates and decimals do; my first cut declared Json and made the same expression a different type depending on whether the query had a FROM clause. A JSON column keeps its declared Json type, unchanged.

Six more wrong answers the tag made visible, all fixed:
- CAST('[1,2]' AS JSON) relabelled instead of parsing, so it still double-encoded when nested.
- doc + 0 became empty where it used to be 3, because as_int and as_float did not read the new variant. Tagging a value must not change what reading it as a number means.
- The implicit default (ALTER TABLE ADD COLUMN backfill, INSERT IGNORE) was untagged text.
- A declared DEFAULT was stored raw and never coerced, so DEFAULT ('[]') double-encoded and DEFAULT ('{') was accepted at CREATE and failed on the first read - exactly the failure the new validation exists to prevent. Coerced at DDL time now, so a bad default is refused by the statement that declares it.
- An expression default that yields text for a JSON column stayed text.
- The image reader accepted any string as a document.

One place the first cut moved away from MySQL and was reverted: JSON_QUOTE returns a string, not a document. Its documented purpose is producing the text of a JSON string literal, so JSON_OBJECT('q', JSON_QUOTE('hi')) is {"q": "\"hi\""} in MySQL 8. Tagging it would have made the function a no-op in the only context it has.

Worth recording about the tests: two of them passed for the wrong reason before being caught. The default test used DEFAULT (JSON_ARRAY()), which produces a tagged value through the expression path and so never exercised the literal default; and the expression-default coercion was invisible until a default that yields text (CONCAT) was added. Both were found by breaking each fix and checking a test failed - eleven mechanisms, all covered.

604 migrations still apply, db pull still gives 209 models, 66 enums and 36 Json fields, conformance unchanged at 99.9 percent, and all three Prisma runner examples pass, which matters here because they round-trip JSON columns through a real driver.
