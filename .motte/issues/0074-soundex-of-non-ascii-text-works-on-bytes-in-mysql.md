---
id: 74
title: SOUNDEX of non-ASCII text works on bytes in MySQL
state: Done
parent: 9
labels: [compat]
created: 2026-08-21T18:55:46Z
updated: 2026-08-26T11:17:39Z
---

## Description

MySQL's SOUNDEX reads its input a byte at a time and emits the first byte it accepts verbatim, so the answer for multi-byte input is not text at all:

    SELECT HEX(SOUNDEX('€abc'))   E2313230   -- 0xE2, then "120"
    SELECT HEX(SOUNDEX('µabc'))   C2313230   -- 0xC2, then "120"
    SELECT HEX(SOUNDEX('🙂abc'))  F0313230

Every one of those leading bytes is the lead byte of a UTF-8 sequence with its continuation bytes dropped — MySQL's my_uni_isalpha is `a-z || A-Z || byte >= 0xC0`, which the lead byte passes and the continuations fail.

crates/engine/src/functions.rs applies that same 0xC0 threshold, but to whole codepoints, because a Rust String cannot hold a lone continuation byte. ASCII agrees exactly; anything above it agrees on the digits and differs on the first character, where this keeps `é` and MySQL sends 0xC3.

Matching it would mean SOUNDEX returning bytes rather than text — Value::Bytes rather than Value::Text — which is a change to what the function's type is for a case nothing is known to need. Recorded rather than done. Found by review during #0066.

## Notes

### 2026-08-26T11:17:39Z — Christopher Vachon (user)

Done in 94703a3, and this issue's premise was wrong -- the recorded values came from a latin1 client.

Over utf8mb4, SOUNDEX('€abc') is E282AC313230 -- all three bytes -- not the E2313230 recorded here. Over a latin1 connection all three recorded values reproduce exactly, because latin1 hands MySQL each UTF-8 byte as its own character and a lead byte is >= 0xC0, so it passes MySQL's letter test while the continuations fail. The bytes looked truncated because it was a different string.

The source says why: Item_func_soundex::val_str branches on rc == 1 && cs->ctype -- decoded to one byte AND the charset has a ctype table. A multi-byte character takes the else branch and is re-encoded whole through cs->cset->wc_mb; it never writes a lone lead byte.

Review challenged this, arguing my_toupper would fold latin1 0xE2 to 0xC2 so a latin1 client could not produce the recorded bytes. It does not: SOUNDEX uses its own soundex_toupper, which is 'a'..'z' only. Demonstrated on 8.3 -- over latin1, é (E9), à (E0) and þ (FE) all stay unfolded while ASCII r folds to R.

Verified exhaustively rather than argued: 74177 codepoints -- every non-surrogate BMP codepoint above 0x7F plus a stride across every astral plane -- sent as raw UTF-8 over a utf8mb4 connection, 0 mismatches against the rule the engine already had. An earlier sweep used ucs2/utf32 introducers; re-run with client bytes on the coordinator's instruction, and both agree.

The real bug was next door, in the binary charset: every byte is rc == 1 with ctype_bin, so only ASCII letters count. The engine decoded those bytes as UTF-8 and applied the text rule, so SOUNDEX(CAST('éabc' AS BINARY)) was é120 against MySQL's A120 -- reachable from a hex literal, a cast, and a VARBINARY column. SOUNDEX(CAST('éé' AS BINARY)) is '' on 8.3 and was é000 here.

Returning Value::Bytes, which this issue named as the price, is not needed: under the binary rule only an ASCII letter is ever emitted, so the result is always ASCII. Had it been needed it would have cost the column type -- Blob where MySQL sends VAR_STRING -- and made SOUNDEX(x) | 0 a refusal.

Digits never differ between the two readings: both code a non-ASCII character as zero, and a zero neither writes nor resets, so only which character lands first is observable.

Also fixed, found by review through the same dispatch: a binary column backfilled by ALTER TABLE held Value::Text, so one column answered a bitwise operator two ways -- 0 for the backfilled row and 1235 for a normally inserted one.

9 mutations, all bite, two of them through the differential. Corpus 1518 -> 1528, baseline empty, verified by re-running the harness independently.
