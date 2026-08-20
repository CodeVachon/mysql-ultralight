---
id: 37
title: JSON is rendered compactly where MySQL spaces it
state: Done
parent: 9
labels: [compat, executor]
created: 2026-08-20T14:21:10Z
updated: 2026-08-20T15:46:23Z
---

## Description

Found by the differential harness. MySQL renders a JSON value with a space after every colon and comma - {"a": [1, 2]} - and the engine renders serde_json's compact form, {"a":[1,2]}.

Every JSON-returning function is affected: JSON_OBJECT, JSON_ARRAY, JSON_EXTRACT, JSON_SET, JSON_MERGE_PATCH, JSON_KEYS, CAST(... AS JSON) and the -> operator. Eleven of the harness's row-value divergences are this one thing.

It parses to the same document either way, so a client reading it as JSON is unaffected. A test asserting the string is not, and that is this engine's whole audience.

The fix is a serialiser that writes MySQL's spacing rather than serde_json::to_string. It will churn every JSON assertion in crates/engine/tests/sql.rs.

## Notes

### 2026-08-20T15:46:23Z — Christopher Vachon (user)

Fixed. Probing MySQL first turned one divergence into three.

The spacing the issue described: a space after every comma and every colon.

The key order, which the issue did not know about and which matters more: MySQL orders an object's keys by byte length first and then bytewise, where serde_json's map is lexicographic. JSON_OBJECT('z',1,'a',2,'yy',3) is {"a": 2, "z": 1, "yy": 3} there and was {"a":2,"yy":3,"z":1} here - a different order, not only a different spelling.

And normalisation on storage: MySQL holds JSON parsed, so '{"b":1,  "a":2}' reads back as {"a": 2, "b": 1}. The engine returned the text as written, which made every column a record of how it happened to be typed.

All three fall out of json::render_text plus re-rendering in SqlType::coerce. String escaping is delegated to serde_json and matches, checked rather than assumed. Duplicate keys keep the last, as MySQL does.

An image written before this holds unnormalised text, so the same data would read back differently depending on whether it arrived through an INSERT or a reload. The image reader already parsed tag 5 to validate it, so it now renders what it parsed - no format bump, and old images load correct.

Three from review:
- JSON_UNQUOTE still emitted the compact form, so SELECT doc, doc->>'$.a' printed two spellings of the same shape in one row. Same file, one line.
- The doc comment claimed number formatting had been checked against MySQL. It had not been checked at the values where it fails: serde writes 1e+30 where MySQL writes 1e30, and switches to exponent form earlier, so 1e-7 is 0.0000001 there. MySQL stays fixed for exponents in [-15, 14]. Recorded as #0046 with a corpus case that keeps it visible, rather than guessed at.
- #0045 was pinned by nothing. It is now: a corpus case shows kept = '{"a": 2, "b": 1}' answering 1 here and 0 in MySQL.

The mechanical rewrite of the test literals reformatted '[9,9]' in two files - the content of a VARCHAR column, embedded as a string, not a document. Caught by the tests that then failed.

The harness reports all eleven JSON row divergences fixed with none new. 604 migrations still apply, db pull byte-identical.
