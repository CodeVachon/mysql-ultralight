---
id: 247
title: A statement too large to parse still aborts the process
state: Todo
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T09:47:08Z
updated: 2026-09-04T09:47:08Z
---

## Description

The remainder of #0218, which is advanced but NOT closed. Filed separately so #0218 can record what landed while this records what is still reachable.

A chain of ~70000 terms (~420 KB of SQL) still aborts the process, and it now dies inside sqlparser's own parse/drop BEFORE the engine has an AST to measure -- so the depth cap that #0218 added cannot see it. The bar moved from a ~7 KB statement to a ~420 KB one; it did not go away.

    engine  fatal runtime error: stack overflow, SIGABRT -- takes the connection and the process
    mysql   1436 Thread stack overrun, or 1473 for select nesting

Two things are needed and neither is small:
  1. a PRE-PARSE bound -- a byte or token limit applied before Parser::parse_sql, or a sqlparser change to bound its own recursion
  2. the PREPARE path: the cap is in Engine::execute only, because adding it to Engine::describe was measured and overflowed the deep-nest test

Priority 2 because it is a process abort reachable from a client, which is worse in kind than any wrong answer -- it takes every other connection with it.
