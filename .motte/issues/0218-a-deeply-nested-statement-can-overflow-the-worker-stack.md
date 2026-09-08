---
id: 218
title: A deeply nested statement can overflow the worker stack
state: Done
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T02:00:21Z
updated: 2026-09-08T13:47:56Z
---

## Description

Found while implementing #0204, and it is now a hard constraint on the codebase rather than a latent one.

Adding ANY field to Session overflows the 2 MB stack in a_deep_nest_of_scalar_subqueries_still_answers. Measured on pristine HEAD: size_of::<Session>() was 408; a bare `pad: [u8; 8]` field with no other change overflowed; `[u8; 1]` did not, because it fits the existing padding. There was literally nothing left.

Lowering SUBQUERY_DEPTH_LIMIT does NOT help -- the hog is the unguarded EXECUTION recursion, not the parse depth. The engine has no stack-growth guard anywhere.

#0204 paid for its diagnostics area by boxing Session::transaction (freeing 64 bytes for a 40-byte area) and added a_session_stays_within_its_stack_budget asserting size_of::<Session>() <= 408, so the NEXT field to be added fails on that assertion with an explanation rather than as a stack overflow in an unrelated test file. That is a tripwire, not a fix.

The real problem: a deeply nested statement from a client can overflow a 2 MB tokio worker stack, which takes down the connection rather than answering 1235. MySQL answers 1436 'Thread stack overrun' or refuses at parse depth. Either a stack-growth guard (the stacker crate is the usual answer) or an execution-depth limit measured against 8.3's actual nesting behaviour.

Priority 2 because the next person to add a Session field is blocked by it, and because a client can reach it.

## Notes

### 2026-09-04T09:46:48Z — Christopher Vachon (user)

ADVANCED, NOT CLOSED, by the ROUND/stack work. A client can still abort the process, so this stays open.

What 8.3 does (measured): BOTH mechanisms. 1473 'Too high level of nesting for select' at 63 levels of scalar-subquery nesting; 1436 'Thread stack overrun' for a long flat chain somewhere between 1000 and 20000 terms. It NEVER refuses 1 AND 1 AND ... at 20000 -- it flattens AND into an n-ary condition.

A stacker red-zone guard in expr::eval was tried FIRST AND MEASUREMENT KILLED IT: the projection type walkers (computed_column_type, never_null, unresolved_column and ~20 siblings) run to completion BEFORE eval is entered, at roughly 12 KB a term against eval's 1.8 KB, so no guard inside the evaluator is ever reached in time. The dependency was removed rather than left in place.

What landed: a measured depth cap at the door -- a self-bounding sqlparser Visitor that Breaks at the limit (so the check cannot out-recurse the limit it enforces) plus an iterative left-spine count for set operations, refusing 1436. MAX_EXPRESSION_DEPTH = 64, because the walkers take a 2 MB thread at ~90 terms in a test build and ~3500 in release, and because it is the same order as MySQL's MAX_SELECT_NESTING of 63.

Every shape between 65 and 50000 terms now answers 1436 on a live connection where it previously aborted. WHAT REMAINS:
  1. a chain of ~70000 terms (~420 KB of SQL) still aborts, now inside sqlparser's own parse/drop BEFORE the engine has an AST to measure. Needs a pre-parse bound or a sqlparser change.
  2. the PREPARE path is uncovered -- adding the check to Engine::describe was measured and overflowed the deep-nest test, so the cap is in execute only.

THE TRIPWIRE'S PREMISE IS STALE: size_of::<Session>() is 384 bytes today, not 408, and a dummy [u8; 4096] field leaves the deep-nest test passing where [u8; 16384] overflows. That came from the tree shrinking, not from this change. Note for anyone touching correlated queries: #0230 carries an enclosing row into a correlated ON, which adds stack per nesting level and lowers the ~90-term debug ceiling the cap of 64 is measured against.

### 2026-09-04T21:48:29Z — Christopher Vachon (user)

#0247 is now closed, which removes the abort this issue's note described as remaining: a connection thread gets 32 MB, so sqlparser survives long enough for the depth cap to refuse the statement. Measured: 70000 and 100000 terms both answer 1436 where they previously lost the connection.

What remains for THIS issue is unchanged and is a divergence rather than a crash: 8.3 answers those statements and the engine refuses them at 64 levels. Also still open, from #0223's note: the depth check is in Engine::execute only, because adding it to Engine::describe overflowed the deep-nest test -- so the prepare path is uncovered.

### 2026-09-08T13:47:56Z — Christopher Vachon (user)

BOTH REMAINING ITEMS ARE DONE. Closing.

1. THE PREPARE PATH IS NO LONGER UNCOVERED (fa9f9a5). The bound was in `Engine::execute` only, because adding it to `Engine::describe` had been measured to overflow the twenty-level nesting test on a 2 MB `cargo test` thread. #0247 removed that obstacle. The gap was real and it was the usual path: measured before the fix, a client that PREPARED a 50000-term statement got `IoError { server disconnected }` where executing the same text answered 1436, and prepared is the path Prisma takes for every statement. Now 300, 5000 and 50000 all answer 1436 at prepare.

Review caught that the returned guard was dropped at the semicolon, which flips DEPTH_MEASURED straight back and re-walks a view definition from a stack already inside the probe. `OutermostStatement` is now `#[must_use]`, so the next dropped guard fails the build; mutate.sh confirms clippy refuses the dropped form.

2. THE DIVERGENCE IS CLOSED FOR THE SHAPES THAT REACH IT (93f9726). This was recorded as "8.3 answers those statements and the engine refuses them at 64 levels" and treated as cosmetic. It was not: measured against 8.3, `findMany({ where: { OR: [ ...80 conditions ] } })` -- a plain Prisma query -- was answered by MySQL and refused 1436 by the engine. The limit of 64 was fitted to a 2 MB `cargo test` thread, not to the 32 MB a connection has had since #0247.

MAX_EXPRESSION_DEPTH is now 256, measured on a connection thread: a debug build answers 1200 terms and loses the connection at 1500, release answered 16000, and per-level cost is dominated by the walker frames rather than by the terms -- correlated scalar subqueries and CONCAT over joined columns abort at the same point as `1 AND 1`, so the margin does not erode with shape.

TWO WRONG TURNS, BOTH FOUND BY REVIEW, BOTH WORTH REMEMBERING:
  - The first attempt exempted flat chains from the depth count and bounded their LENGTH separately. The two bounds composed: a chain whose leftmost term is a parenthesised chain pays one level of depth and 256 terms of stack per level, so six levels passed both bounds at ~1500 terms and ABORTED THE PROCESS. Reverted.
  - It also had to classify operators, and got it wrong: `||` (which this engine evaluates as OR under the default sql_mode) and `XOR` were left out, so the divergence survived for those spellings.
  - The same composing-bounds mistake was then found in code that predates all of this: `set_operation_spine` was a second budget reusing the same constant without adding to `at`, re-granted at every nested query. A 256-arm spine whose leftmost arm carries a 250-term chain passed both. It is folded into the one budget now.

The lesson is one line: a flat chain costs exactly as much stack as nesting, because `a OR b OR c` IS a tree of depth three. Anything that costs frames goes into the same number or it composes with everything that does.

WHAT IS NOT CLOSED, and is now #0253: the limit is measured against the stack the BINARY gives a connection, because that is the only way the engine ships. Driven as a library on a 2 MB thread it can now abort between roughly 90 and 256 levels where at 64 it refused. Also: nothing in the suite exercises a 2 MB stack any more, so a future engine test that writes a long chain and forgets to spawn a fat thread aborts the binary rather than failing, and the gate does not flag it.

Still divergent from 8.3 above 256 levels, deliberately: 8.3 answers a 20000-term chain and this refuses it. That is the house rule's trade -- a clear error rather than a wrong answer, and rather than a dead connection.

Differential 5544 statements, 0 divergences, with a new group covering 70-term OR and AND chains, compound terms, a chain in a projection, and the `||`/`XOR` spellings.
