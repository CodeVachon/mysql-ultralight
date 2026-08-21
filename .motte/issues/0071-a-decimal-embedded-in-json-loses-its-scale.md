---
id: 71
title: A decimal embedded in JSON loses its scale
state: Todo
parent: 9
labels: [compat]
created: 2026-08-21T16:09:17Z
updated: 2026-08-21T16:09:17Z
---

## Description

DECIMAL(10,2) holding 2.50
    SELECT JSON_OBJECT('a', a)   mysql {"a": 2.50}   engine {"a": 2.5}

Found by review during #0049. crates/engine/src/json.rs parses the decimal's text into a serde_json::Number, and serde_json is built without the arbitrary_precision feature, so every number becomes an f64 and is written back out shortest-form. The scale the tag exists to carry is dropped at the boundary.

Turning arbitrary_precision on makes serde_json hold a number as its source text, which would fix this — and is worth looking at for #0046 at the same time, where the complaint is that JSON doubles outside a middling range are written differently from MySQL. It is not a free switch: Number stops being a plain enum over i64/u64/f64, so crates/engine/src/json.rs's comparison and rendering both have to be re-read against it.

Not in the differential baseline; the corpus has no decimal inside a document.
