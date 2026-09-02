---
id: 108
title: The image tests collide in the shared temp directory
state: Done
parent: 9
labels: [testing]
created: 2026-08-25T17:00:04Z
updated: 2026-09-02T18:52:39Z
---

## Description

Found by review during #0059, pre-existing.

crates/engine/tests/image.rs writes to fixed paths in the shared system temp directory (mu-image-*.mus), so two concurrent runs of the suite delete each other's images and describing_never_snapshots_restores_or_takes_a_lock fails. The reviewer hit it by having two scripts/check.sh runs overlap.

A test that fails because another copy of itself is running is the worst kind of flake to debug: the failure names a snapshot behaviour that has nothing to do with the cause, and it cannot be reproduced by running that test alone. Whoever hits it next will look at the lock logic.

Fix by giving each run its own directory -- the process id, or a temp-dir crate -- rather than a fixed name. Worth checking whether any other test in the workspace shares the same habit.

## Notes

### 2026-09-02T18:52:39Z — Christopher Vachon (user)

Done in 6cc9fc1, in an isolated worktree.

A scratch directory per test named from the pid, the clock and a counter: the pid separates concurrent runs, the counter separates tests within one run, and the clock separates a run from a directory left by a killed process whose pid has been reused. Removed whole on Drop, which covers the panic path the nine hand-written cleanups did not. No new dependency -- cargo audit and cargo machete would carry one forever for a directory name and a remove_dir_all.

The leftover scan had the same bug it was testing for: it read all of the temp directory for mu-image-*.tmp, so another run's in-flight temporary counted as this run's leak. It now reads only its own directory.

Found and fixed a dead assertion in the same file: a_snapshot_is_written_whole_or_not_at_all checked for atomic.tmp, a name the implementation never writes -- it writes atomic.mus.<pid>.tmp -- so it could not fail under any behaviour. Replaced with one asserting the scratch directory holds nothing but the image, and mutation 4 (rename swapped for copy) fails it where the old spelling passed.

Evidence is the pair, not a single run: two concurrent cargo test --workspace invocations fail on the unmodified code with two DIFFERENT unrelated-looking assertions, and four overlapping pairs pass after. Re-verified independently on merge -- two concurrent image suites, 30 passed each.

Swept the workspace for the same habit: only this file had it. wire.rs and cli.rs bind port 0; the npm tests are already pid-scoped or use mkdtempSync; examples/app's image cache is content-addressed inside the checkout. Noted but not touched: that cache's stale-pruning could delete a concurrently running suite's image within one checkout if the two runs have different binaries.

The cross-process half is not expressible as a mutation -- one process cannot observe that another's pid differs -- so it is asserted in-process (two Scratch values never collide, and the directory does not outlive the test) and shown by the transcripts.

No corpus cases: nothing the engine answers with changed.
