---
id: 218
title: A deeply nested statement can overflow the worker stack
state: In Progress
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T02:00:21Z
updated: 2026-09-04T04:49:43Z
---

## Description

Found while implementing #0204, and it is now a hard constraint on the codebase rather than a latent one.

Adding ANY field to Session overflows the 2 MB stack in a_deep_nest_of_scalar_subqueries_still_answers. Measured on pristine HEAD: size_of::<Session>() was 408; a bare `pad: [u8; 8]` field with no other change overflowed; `[u8; 1]` did not, because it fits the existing padding. There was literally nothing left.

Lowering SUBQUERY_DEPTH_LIMIT does NOT help -- the hog is the unguarded EXECUTION recursion, not the parse depth. The engine has no stack-growth guard anywhere.

#0204 paid for its diagnostics area by boxing Session::transaction (freeing 64 bytes for a 40-byte area) and added a_session_stays_within_its_stack_budget asserting size_of::<Session>() <= 408, so the NEXT field to be added fails on that assertion with an explanation rather than as a stack overflow in an unrelated test file. That is a tripwire, not a fix.

The real problem: a deeply nested statement from a client can overflow a 2 MB tokio worker stack, which takes down the connection rather than answering 1235. MySQL answers 1436 'Thread stack overrun' or refuses at parse depth. Either a stack-growth guard (the stacker crate is the usual answer) or an execution-depth limit measured against 8.3's actual nesting behaviour.

Priority 2 because the next person to add a Session field is blocked by it, and because a client can reach it.
