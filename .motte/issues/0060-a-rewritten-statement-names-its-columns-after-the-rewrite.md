---
id: 60
title: A rewritten statement names its columns after the rewrite
state: In Progress
parent: 9
assignee: Christopher Vachon
labels: [compat]
created: 2026-08-21T02:24:55Z
updated: 2026-08-25T20:18:15Z
---

## Description

crates/engine/src/exec.rs publishes the *normalised* text for column naming, so under a sql_mode that makes crates/engine/src/rewrite.rs change the statement, the name a client gets is one it never wrote:

    sql_mode=ANSI_QUOTES;  SELECT "x" + 1 FROM P   engine  `x` + 1   mysql  "x" + 1
    sql_mode=IGNORE_SPACE; SELECT COUNT (*)        engine  COUNT(*)  mysql  COUNT (*)

Found by review during #0044. The whole point of naming from the source is that a client keys a row by the name, so handing back the rewritten form defeats it for exactly the modes the rewriter exists to serve.

Not fixed with an offset heuristic on purpose. The spans point into the normalised text, and the obvious repair — slice the original at the same offsets when the two are the same length — is only sound because double_quotes_to_backticks happens to be positional; it is not length-preserving in general ("" collapses and a backtick doubles), so a pathological statement would silently produce a name from the wrong offsets. That is the failure the round-trip guard exists to prevent, so doing this properly means the rewriter reporting an offset mapping rather than just a string.
