---
id: 60
title: A rewritten statement names its columns after the rewrite
state: Done
parent: 9
assignee: Christopher Vachon
labels: [compat]
created: 2026-08-21T02:24:55Z
updated: 2026-08-25T21:50:56Z
---

## Description

crates/engine/src/exec.rs publishes the *normalised* text for column naming, so under a sql_mode that makes crates/engine/src/rewrite.rs change the statement, the name a client gets is one it never wrote:

    sql_mode=ANSI_QUOTES;  SELECT "x" + 1 FROM P   engine  `x` + 1   mysql  "x" + 1
    sql_mode=IGNORE_SPACE; SELECT COUNT (*)        engine  COUNT(*)  mysql  COUNT (*)

Found by review during #0044. The whole point of naming from the source is that a client keys a row by the name, so handing back the rewritten form defeats it for exactly the modes the rewriter exists to serve.

Not fixed with an offset heuristic on purpose. The spans point into the normalised text, and the obvious repair — slice the original at the same offsets when the two are the same length — is only sound because double_quotes_to_backticks happens to be positional; it is not length-preserving in general ("" collapses and a backtick doubles), so a pathological statement would silently produce a name from the wrong offsets. That is the failure the round-trip guard exists to prevent, so doing this properly means the rewriter reporting an offset mapping rather than just a string.

## Notes

### 2026-08-25T21:50:56Z — Christopher Vachon (user)

Done in 2517c12.

The rewriter now reports an offset mapping rather than just a string, which is what this issue said doing it properly would take. Every pass writes through a Rewriter recording each stretch as verbatim -- an offset means the same on both sides -- or as a rewrite, which has two lengths and no addressable interior. Composition happens as each pass copies, so a span in the final text carries back through all six passes.

A range answers as the smallest contiguous original range covering it, and that single property settles the cases this issue worried about: text removed between two mapped stretches falls inside the hull automatically, so COUNT (*) keeps its space, while a removal at either edge stays outside, so a range stopping at COUNT does not collect a trailing one. It also means a pure deletion needs no piece at all -- discovered because the piece written for one refused to bite under mutation, not by reasoning.

The naming walk had to ask the same question of the original text. close_call_spaces closes a gap after any alphanumeric run, so SELECT ( became SELECT( and the backward walk would not cross the T it had just been handed. A keyword list cannot fix that -- LEFT, IF and COUNT are keywords and function names both, the same trap mod_operator_to_percent documents -- so starts_a_token now accepts a boundary in either text, which also fixes any future pass that closes a gap.

Measured on 8.3: IGNORE_SPACE is about function names only, so the gap after SELECT was never the mode's doing; it was purely our rewrite's.

Review fuzzed 4000 statements across four modes: 3185 wrong names at HEAD, 626 after the first version, 0 of 3787 describable after the finding-1 fix. It also proved all six converted passes byte-identical to the previous implementation over 44000 statements, and the mapping's invariants over 20000.

The corpus had read green because its only parenthesised item sat third, behind a comma, which is exactly the position where the bug cannot fire. Leading-position cases added, and the other new cases re-checked for the same blindness.

respell_mod and Normalised::mod_operators are gone -- MOD is now just another rewrite the mapping reverses, which is what 8bacd5d's bespoke machinery was standing in for.

#0109 is half unblocked: the mapping it waited on exists, but params::bind splices values before normalise runs, so naming SELECT ? is now a binder change rather than a naming change. Noted there.

15 mutations, all bite. Corpus 1216 -> 1266, baseline empty.
