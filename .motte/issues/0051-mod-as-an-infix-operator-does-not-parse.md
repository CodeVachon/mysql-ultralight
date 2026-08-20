---
id: 51
title: MOD as an infix operator does not parse
state: Todo
parent: 9
labels: [compat]
created: 2026-08-20T19:24:50Z
updated: 2026-08-20T19:24:50Z
---

## Description

Split out of #0042, which added DIV. SELECT n MOD 3 fails with 1064 "sql parser error: Expected: end of statement, found: 3". The % spelling and the MOD(a,b) function form both work; only the infix keyword does not.

sqlparser 0.62's MySqlDialect::parse_infix handles DIV and nothing else, so this is a parser gap rather than a missing evaluation. The engine cannot fix it with a wrapper Dialect — see crates/engine/src/dialect.rs for why — which leaves a text or token level rewrite in crates/engine/src/rewrite.rs.

That rewrite is not obviously safe. `MOD(7,2)` is a function call and `7 MOD (2)` is the infix form, and the two are distinguished only by what precedes the MOD: an operand means infix, anything else means a call. A token-level pass using sqlparser's own tokenizer could do it correctly; a naive text replace would turn `SELECT a mod FROM t` (an alias) into a syntax error.

Not built because the conformance corpus uses the infix form zero times, and the current behaviour is a clear parse error rather than a wrong answer. Worth doing if a target codebase turns out to use it.
