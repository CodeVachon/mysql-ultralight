---
id: 164
title: check.sh exits non-zero on its own skip path
state: Done
parent: 9
labels: [testing]
created: 2026-09-03T04:07:42Z
updated: 2026-09-03T11:12:06Z
---

## Description

Found while implementing #0158, latent and pre-existing.

scripts/check.sh around line 175 echoes a message containing $d inside double quotes on the path taken when a check is SKIPPED. Under set -u the variable is expanded and unset, so the script aborts before printing its verdict -- a skip becomes a non-zero exit with no explanation.

Not hit in normal use because the skip path needs a missing tool, which is exactly the situation --require-tools exists to turn into a failure. But it means the one branch designed to degrade gracefully is the one that crashes, and it would first be seen on a fresh clone missing cargo-machete or cargo-audit, where the message is most needed.

Trivial to fix; recorded because the failure it produces looks like a check failing rather than the script itself breaking.

## Notes

### 2026-09-03T11:12:06Z — Christopher Vachon (user)

Landed in 2b925b3. Reproduced first: the double-quoted install hint expanded $d under set -u, so a skip exited 127 with no verdict. Confirmed fixed by running the verdict block with SKIPPED non-empty -- prints and exits 0. Independently hit by the #0150 agent in a fresh worktree, which is exactly the fresh-clone path the issue predicted.
