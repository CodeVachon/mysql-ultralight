---
id: 218
title: A deeply nested statement can overflow the worker stack
state: In Progress
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T02:00:21Z
updated: 2026-09-04T09:46:48Z
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
