---
id: 253
title: The engine on a small stack can abort where the server refuses
state: Todo
parent: 9
labels: [executor, app-priority-3]
created: 2026-09-08T13:01:33Z
updated: 2026-09-08T13:22:48Z
---

## Description

Split out of #0218 when the expression-depth limit was raised, so the trade is recorded rather than buried in a test comment.

MAX_EXPRESSION_DEPTH is 256, measured against the stack the *binary* gives a connection -- 32 MB since #0247 -- because that is the only way the engine is shipped: the npm platform packages spawn the CLI.

Driven as a library on a thread with the 2 MB a `cargo test` thread gets, the walkers take the stack at about ninety terms. So a statement between roughly 90 and 256 levels deep now ABORTS THE PROCESS on such a thread, where at the old limit of 64 it was refused 1436.

Measured 2026-09-08, debug build, on a connection thread: 1200 plain terms answer and 1500 lose the connection; 500 terms of the fatter shape Prisma emits still answer. A release build answered 16000. On 2 MB the figure is about 90, which is what the old limit of 64 was fitted to.

Why it was given up: holding the shipped configuration to a limit measured on a stack it never runs with refused ordinary application queries. `findMany({ where: { OR: [ ...80 conditions ] } })` was answered by 8.3 and refused 1436 by the engine.

A SECOND, SHARPER CONSEQUENCE, from review: nothing in the suite exercises a 2 MB stack any longer, because the test that used to is now spawned at 32 MB. A future engine test that writes a long chain and forgets to spawn a fat thread does not fail an assertion -- it aborts the whole test binary with SIGABRT, and nothing in the gate flags it. Measured: `SELECT 1 AND ... (256 terms)` on a default `cargo test` thread does exactly that.

What would close this: the limit is really a property of the stack the caller provides, and only the caller knows it. Either the engine takes the bound from its configuration and the CLI sets it from CONNECTION_STACK, or every engine entry point is documented as requiring a stack of a stated size. A reading of the remaining stack was tried for #0218 and rejected -- it makes the limit move between builds, which is a limit nobody can write a test for.

Priority 3: the shipped path is the binary, and every test that walks a deep chain already spawns a 32 MB thread. This is a gap in what the engine promises an embedder, not one the target application reaches.
