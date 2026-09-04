---
id: 204
title: There is no diagnostics area, so every warning is invisible
state: Done
parent: 9
labels: [compat, app-priority-2]
created: 2026-09-03T17:51:23Z
updated: 2026-09-04T02:18:15Z
---

## Description

Found while implementing #0194/#0195, and it is the natural successor to #0168.

SHOW WARNINGS is 1235. After #0194/#0195 the engine stores what MySQL stores under a non-strict mode -- truncating a string, clamping an integer -- but says nothing about having done it, where 8.3 records 1265 or 1264 as a warning the client can read.

So WHAT is stored now matches; what is REPORTED about it does not. Cases already known to raise a warning on 8.3 and nothing here:
  1265 data truncated (a non-strict over-long write, and a trailing-space trim under any mode)
  1264 out of range (a non-strict clamp)
  1366 incorrect value (#0200)
  1466 identifier cut (#0168)

#0168 filed this as the absence of a warning channel from the alias side. This is the same absence seen from the write side, and it is now reachable on ordinary INSERTs rather than only on an exotic alias, which is why it is priority 2. Whoever takes it should do #0168 in the same pass.

## Notes

### 2026-09-04T02:18:15Z — Christopher Vachon (user)

Landed in 8b42710, gate green. The lifetime rules were the part that could not be guessed, all measured: a statement REPLACES the area (with nothing, when it raised nothing), so a bare SELECT between a write and SHOW WARNINGS empties it; the SHOW forms do NOT clear and read the same list twice; SELECT @@warning_count is not a reader -- it answers the previous statement's count and then clears, so three reads give 1, 0, 0; a failing statement leaves its error APPENDED AFTER whatever it raised first; and @@max_error_count caps the list but not the counts.

One code carries two levels: a VARCHAR(3) given 'abc  ' keeps every non-space character and gets a NOTE, the same column given 'abcdef' gets a WARNING. A CHAR(3) given 'ab   ' raises nothing at all -- padding is not truncation, which is why the binary raise sits inside the truncation branch and not after the pad.

3156 (invalid JSON value for CAST) is deliberately NOT implemented: it needs a per-RESULT-ROW counter threaded through several evaluation paths and fires from plain j + 0 as well as CAST, so it is a mechanism on top of this channel rather than a raise site.

#0200 is now nearly trivial and was left as instructed -- it is a lenient() branch plus a raise, but it changes what is STORED, so it belongs to whoever owns that call.
