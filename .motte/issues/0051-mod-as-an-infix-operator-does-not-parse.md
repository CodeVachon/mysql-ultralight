---
id: 51
title: MOD as an infix operator does not parse
state: Done
parent: 9
labels: [compat]
created: 2026-08-20T19:24:50Z
updated: 2026-08-24T15:19:55Z
---

## Description

Split out of #0042, which added DIV. SELECT n MOD 3 fails with 1064 "sql parser error: Expected: end of statement, found: 3". The % spelling and the MOD(a,b) function form both work; only the infix keyword does not.

sqlparser 0.62's MySqlDialect::parse_infix handles DIV and nothing else, so this is a parser gap rather than a missing evaluation. The engine cannot fix it with a wrapper Dialect — see crates/engine/src/dialect.rs for why — which leaves a text or token level rewrite in crates/engine/src/rewrite.rs.

That rewrite is not obviously safe. `MOD(7,2)` is a function call and `7 MOD (2)` is the infix form, and the two are distinguished only by what precedes the MOD: an operand means infix, anything else means a call. A token-level pass using sqlparser's own tokenizer could do it correctly; a naive text replace would turn `SELECT a mod FROM t` (an alias) into a syntax error.

Not built because the conformance corpus uses the infix form zero times, and the current behaviour is a clear parse error rather than a wrong answer. Worth doing if a target codebase turns out to use it.

## Notes

### 2026-08-24T15:19:55Z — Christopher Vachon (user)

Done in 8bacd5d. The rewrite runs over sqlparser's own tokens: MOD is the operator only where the previous significant token ends an operand and the next begins one. That test is necessary and not sufficient -- ALTER TABLE t CHANGE colx mod GEOMETRY has an operand on both sides, and GEOMETRY, SERIAL, FIXED and the spatial types are not sqlparser keywords at all, so no keyword list separates them. The grammar does: a statement that stops parsing after a substitution is handed back unrewritten.

Naming had to come with it. normalise now returns the rewrite sites, and exec.rs puts the client's own spelling and case back into an unaliased column's name -- the first differential run failed on 14 column names reading n % 3 off the rewritten text.

Not covered, deliberately: a left operand that is a bare keyword (year MOD 4) and a right operand that is a function call (n MOD ABS(x)) still hit the old parse error. Widening ends_an_operand to accept keyword words is what makes the DDL cases unsafe.

Corpus 256 -> 260 statements, baseline still empty.
