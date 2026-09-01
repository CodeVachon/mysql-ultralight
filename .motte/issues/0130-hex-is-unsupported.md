---
id: 130
title: HEX is unsupported
state: Done
parent: 9
labels: [executor]
created: 2026-08-26T11:18:03Z
updated: 2026-08-31T21:18:30Z
---

## Description

SELECT HEX('abc') raises 1235. MySQL answers 616263.

Found while implementing #0074, where it was the direct obstacle: every byte-level measurement in that issue had to be run against MySQL alone, because no differential case could express HEX(SOUNDEX(...)) -- the one function that makes a byte-level answer visible at all.

That is the argument for doing it beyond the function itself: without HEX, no corpus case can check what any function returns at the byte level, so a whole class of divergence is invisible to the harness. #0074's binary-charset bug was found by reading MySQL's source and probing by hand, not by the corpus, and HEX is why.

Check UNHEX, TO_BASE64/FROM_BASE64 and BIN in the same pass, and what each returns for a binary argument as against a text one.

## Notes

### 2026-08-31T21:18:30Z — Christopher Vachon (user)

Done in 89b225a. HEX, UNHEX, BIN, TO_BASE64 and FROM_BASE64.

The deliverable was the corpus being able to see bytes, not the function. #0074's binary-charset measurements are now green cases -- HEX(SOUNDEX('€abc')), HEX(SOUNDEX('µabc')), HEX(SOUNDEX(CAST('éabc' AS BINARY))) -- rather than hand-probes against MySQL alone.

Measured rules worth keeping. HEX has two readings picked by the argument's declared type: a string hexes its bytes, a number hexes its value. The rounding differs by type too -- a DECIMAL 1.5 rounds half away from zero, a DOUBLE 2.5e0 half to even. UNHEX of an odd-length string prepends a zero nibble, so UNHEX('a') is 0x0A rather than NULL or 0xA0; it is case-insensitive in and byte-exact out. TO_BASE64 wraps after every 76 characters with no trailing newline. FROM_BASE64 skips whitespace anywhere, requires a multiple of four, and stops at padding -- 8.3.0 does not pass ALLOW_MULTIPLE_CHUNKS, so FROM_BASE64('YQ==YQ==') is NULL, measured six ways after review claimed otherwise. BIN is CONV(N,10,2) and reads the characters its argument prints as, so BIN(1e30) is 1 and BIN over a DATE is its year, not the packed form; it is unsigned with wrapping negation and saturates asymmetrically.

Refused rather than guessed: BIN over a binary string (CONV takes a hex literal's number and parses a VARBINARY column's bytes as digits, and both are Value::Bytes here -- see #0141, which would turn the refusal into an answer), and HEX of a DOUBLE outside the BIGINT range, which is 1690 as MySQL answers for every computed form.

UNHEX and FROM_BASE64 are the first computed columns this engine describes as bytes. Review found the flag surviving only the four OutColumn::new sites and lost through everything rebuilt from an SqlType -- derived table, CTE, view, CTAS -- while the row still carried Value::Bytes, handing a client raw non-UTF-8 under a utf8mb4 column, which is the mangling this issue exists to stop. Fixed at the one point they share, widening to varbinary(512): varbinary and not blob or binary because only that spelling keeps the wire type VAR_STRING that 8.3 sends, and MySQL's own view over UNHEX('4142') is declared varbinary(8).

Review also caught a comment claiming the byte tag made UNHEX('4142') = 'ab' answer 0. It does not -- a Bytes/Text pair still compares through the collation, filed as #0139 -- and the tag's real justification is that a String cannot hold UNHEX('FF') and a lossy decode makes HEX(UNHEX('FF')) answer EFBFBD.

The surprising measurements were in unit tests only, against hand-written expectations. They are now corpus cases, and all of them matched 8.3 on the first run.

For #0128: HEX localises it. Three of its four measurements agree through HEX -- the bytes survive and only the charset label is lost -- narrowing it to case-folding and the column-charset comparison. Its SOUNDEX(UPPER(b)) case is now askable directly: HEX(UPPER(b)) over 'éabc' is C3A9616263 on 8.3 and C389414243 here.

38 mutations. Two initially did not bite and both were fixed by strengthening the test rather than accepting them. Corpus 1820 -> 1904, baseline empty, both gates re-run independently.

Filed: #0139, #0140, #0141, #0142, #0143.
