---
id: 18
title: Parser must be sql_mode aware (ANSI_QUOTES, PIPES_AS_CONCAT)
state: Todo
parent: 4
labels: [compat, parser, blocker]
created: 2026-08-18T15:29:25Z
updated: 2026-08-18T15:37:17Z
---

## Description

Discovered during Track A. The target deployment does not run MySQL's default sql_mode. provisions/mysql/my.cnf sets:

ANSI_QUOTES, ONLY_FULL_GROUP_BY, ERROR_FOR_DIVISION_BY_ZERO, IGNORE_SPACE, NO_ENGINE_SUBSTITUTION, NO_ZERO_DATE, NO_ZERO_IN_DATE, PIPES_AS_CONCAT, REAL_AS_FLOAT, STRICT_ALL_TABLES

Three of these change how SQL is lexed and parsed, not merely how it behaves at runtime:

ANSI_QUOTES makes the double quote an identifier quote rather than a string delimiter. Under this mode "foo" is a column, not a string. Getting it wrong does not raise an error, it silently changes what a query means.

PIPES_AS_CONCAT makes || string concatenation rather than logical OR. Same failure shape: a valid parse with the wrong semantics.

IGNORE_SPACE permits whitespace between a function name and its opening parenthesis, which also makes those function names reserved words.

REAL_AS_FLOAT and STRICT_ALL_TABLES affect the type system and write path rather than the grammar, but still diverge from the defaults every reference assumes.

Consequence: the parser cannot be built against default MySQL and adjusted later. sql_mode must be a parser input from the start, and the engine must accept the mode via configuration so a test process can match production exactly.

Note the spike handler in crates/cli/src/spike.rs currently reports the MySQL 8 default sql_mode, which is wrong for this deployment.

## Notes

### 2026-08-18T15:37:17Z — Christopher Vachon (user)

Confirmed empirically on 2026-08-18 by querying a container built from the repo's my.cnf: @@sql_mode is REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,ONLY_FULL_GROUP_BY,STRICT_ALL_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION.
