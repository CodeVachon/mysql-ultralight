---
id: 33
title: JSON values are text, so nested constructors double-encode
state: Todo
parent: 6
labels: [executor, compat]
created: 2026-08-19T12:47:14Z
updated: 2026-08-19T12:47:14Z
---

## Description

JSON_OBJECT('a', JSON_ARRAY(1, 2)) yields {"a": "[1,2]"} where MySQL yields {"a": [1, 2]}.

The engine has no JSON-typed value: JSON is stored, compared and returned as text, which is how it arrives and leaves on the wire. By the time one JSON function's result reaches another it is an ordinary string, and json::from_value embeds it as one. Nothing distinguishes a document from a string that happens to look like one, so this cannot be detected and refused either - it is the one place the engine can be silently wrong rather than clearly unsupported.

The fix is a distinct Value::Json variant, which touches value.rs, the wire encoding, coercion, comparison, and the state image format (a new tag, so FORMAT_VERSION goes to 2).

Not urgent: nothing in the 604 migrations nests a JSON constructor. Worth doing before anyone relies on building documents from parts.
