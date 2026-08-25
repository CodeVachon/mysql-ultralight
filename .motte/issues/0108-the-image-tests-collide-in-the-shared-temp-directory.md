---
id: 108
title: The image tests collide in the shared temp directory
state: Todo
parent: 9
labels: [testing]
created: 2026-08-25T17:00:04Z
updated: 2026-08-25T17:00:04Z
---

## Description

Found by review during #0059, pre-existing.

crates/engine/tests/image.rs writes to fixed paths in the shared system temp directory (mu-image-*.mus), so two concurrent runs of the suite delete each other's images and describing_never_snapshots_restores_or_takes_a_lock fails. The reviewer hit it by having two scripts/check.sh runs overlap.

A test that fails because another copy of itself is running is the worst kind of flake to debug: the failure names a snapshot behaviour that has nothing to do with the cause, and it cannot be reproduced by running that test alone. Whoever hits it next will look at the lock logic.

Fix by giving each run its own directory -- the process id, or a temp-dir crate -- rather than a fixed name. Worth checking whether any other test in the workspace shares the same habit.
