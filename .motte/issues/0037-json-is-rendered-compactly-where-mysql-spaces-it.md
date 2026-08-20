---
id: 37
title: JSON is rendered compactly where MySQL spaces it
state: Todo
parent: 9
labels: [compat, executor]
created: 2026-08-20T14:21:10Z
updated: 2026-08-20T14:21:10Z
---

## Description

Found by the differential harness. MySQL renders a JSON value with a space after every colon and comma - {"a": [1, 2]} - and the engine renders serde_json's compact form, {"a":[1,2]}.

Every JSON-returning function is affected: JSON_OBJECT, JSON_ARRAY, JSON_EXTRACT, JSON_SET, JSON_MERGE_PATCH, JSON_KEYS, CAST(... AS JSON) and the -> operator. Eleven of the harness's row-value divergences are this one thing.

It parses to the same document either way, so a client reading it as JSON is unaffected. A test asserting the string is not, and that is this engine's whole audience.

The fix is a serialiser that writes MySQL's spacing rather than serde_json::to_string. It will churn every JSON assertion in crates/engine/tests/sql.rs.
