---
id: 71
title: A decimal embedded in JSON loses its scale
state: Done
parent: 9
labels: [compat]
created: 2026-08-21T16:09:17Z
updated: 2026-08-26T03:48:49Z
---

## Description

DECIMAL(10,2) holding 2.50
    SELECT JSON_OBJECT('a', a)   mysql {"a": 2.50}   engine {"a": 2.5}

Found by review during #0049. crates/engine/src/json.rs parses the decimal's text into a serde_json::Number, and serde_json is built without the arbitrary_precision feature, so every number becomes an f64 and is written back out shortest-form. The scale the tag exists to carry is dropped at the boundary.

Turning arbitrary_precision on makes serde_json hold a number as its source text, which would fix this — and is worth looking at for #0046 at the same time, where the complaint is that JSON doubles outside a middling range are written differently from MySQL. It is not a free switch: Number stops being a plain enum over i64/u64/f64, so crates/engine/src/json.rs's comparison and rendering both have to be re-read against it.

Not in the differential baseline; the corpus has no decimal inside a document.

## Notes

### 2026-08-26T03:48:49Z — Christopher Vachon (user)

Done in 5a70d9f, via arbitrary_precision.

The alternative -- carrying exactness in Value::Decimal and reusing decimal.rs -- does not reach, and the reason is structural: the scale has to survive render_text, Value::Json holding a string, and a re-parse, and serde_json::Value has nowhere to keep the digits. decimal.rs still does the exact work: compare, shortest and expanded are what the document comparison and key run on.

Blast radius checked before flipping it: the only other serde_json user in the workspace is crates/conformance, which builds with json! and calls to_string_pretty, so nothing there deserializes.

Three consequences of the switch, all recorded in the code. Serde canonicalises an exponent's sign, so rendering strips the +. Without the feature serde refused an out-of-range number by reading it into an f64, and with it nothing objects, so 1e2000 is refused explicitly. And Number::as_f64 is Rust's correctly-rounded parse where serde's own and MySQL's rapidjson are approximate -- using Rust's moved 1000000000000000.4 to .5, so a double is still read through serde, which is what keeps #0046's cases green.

Review found the real cost of the feature: serde_json hijacks the object key $serde_json::private::Number, so a legal document carrying it was silently rewritten to a scalar or refused outright -- and one inside a state image failed the entire reload. 8.3 treats that key as ordinary in every position, measured across eight shapes. Documents are now read through a deserializer of this crate's own, which distinguishes serde's parked digits from a real member by ownership: a member's string is borrowed from the input or the unescaping scratch buffer, parked digits arrive owned. That holds through an escaped spelling of either, which no scan of the source text could catch, and a unit test asserts both directions so a serde that stopped borrowing fails loudly rather than quietly rewriting documents.

Also fixed: CAST(x AS JSON) over a FLOAT re-read the column's six-significant-digit text, so it disagreed with JSON_ARRAY of the same value; both now convert through from_value and match 8.3.

#0046 unaffected -- the double formatter and its my_gcvt predicate are untouched. #0072 verified still open and deliberately not moved: it is one step earlier, in how the two parsers break a tie, and trading it would have introduced three new gaps.

JSON_TYPE stays DOUBLE for a decimal where 8.3 says DECIMAL, and the reason is now measured rather than assumed: 8.3 renders DECIMAL(10,1) 2.5 and CAST('2.5' AS JSON) identically while typing them differently, so the spelling is provably not a function of the type. The only available partial rule would make the answer depend on the column's scale, which MySQL does not do. Filed as #0121.

17 mutations, all bite. Corpus 1386 -> 1420, baseline empty.
