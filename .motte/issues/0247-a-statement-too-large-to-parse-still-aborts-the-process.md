---
id: 247
title: A statement too large to parse still aborts the process
state: Done
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T09:47:08Z
updated: 2026-09-04T21:48:29Z
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

## Notes

### 2026-09-04T21:48:29Z — Christopher Vachon (user)

Landed in 8636525, gate green.

THE OBVIOUS FIXES WERE MEASURED AND REJECTED, which is the useful part of this. A bound on the statement's SIZE would refuse legitimate SQL: a 731 KB flat INSERT of 20000 rows parses fine and agrees with 8.3, while a 410 KB chain dies -- and the flat one has MORE TOKENS than the chain. Neither bytes nor tokens separate them; only the shape of the parse tree does, which is not knowable without parsing.

So the fix is not a second limit. tokio's default 2 MB thread was never enough to PARSE what #0223's depth cap exists to refuse, so the cap was never reached. At 32 MB the parser survives and the cap does its job: measured against the real server, 70000-term and 100000-term chains both come back as a clean 1436 where they previously gave 'Lost connection to MySQL server during query'.

Threshold before the fix: 60000 terms refused cleanly, 70000 aborted.

The test spawns its own thread at CONNECTION_STACK on purpose -- on a cargo test thread's 2 MB this chain aborts the test binary rather than failing an assertion, which is what the mutation proving it bites demonstrates (SIGABRT).
