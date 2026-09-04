---
id: 157
title: The runner examples fail the gate under concurrent load
state: Todo
parent: 9
labels: [testing, app-priority-none]
created: 2026-09-03T02:40:14Z
updated: 2026-09-04T00:22:02Z
---

## Description

Observed repeatedly while running several agents against one checkout, and it has now cost several gate cycles.

scripts/check.sh's vitest and jest stages fail with 5000 ms hook or test timeouts whenever another cargo build, another example suite, or a mutate.sh loop is running on the same machine. The same suites take 350-650 ms when the box is quiet -- measured: examples/vitest standalone is 350 ms for 3 tests, against a 5 s timeout it exceeded under load.

The failure says nothing about the cause. It names a test that is not slow and gives no hint that the machine was busy, so the reader re-runs it, sees it pass, and learns to distrust the gate -- which is the same damage #0108's temp-directory collision did, from a different direction. A gate that fails for reasons unrelated to the change is worse than a slow one.

Options worth measuring before choosing: raise the runner timeouts (cheapest, but hides a genuinely slow suite); run the example suites with a concurrency limit; or have check.sh serialise its npm stages. Note CI runs on a quiet box, so this is a local-development problem specifically -- which is where the gate is run most often.

Related: #0108 (image tests collided on fixed temp paths, now fixed) is the same class -- a test failing because of something other than the code under test.

## Notes

### 2026-09-04T00:19:03Z — Christopher Vachon (user)

LIKELY ROOT CAUSE FOUND 2026-09-03, and it is not concurrency.

A gate run hung indefinitely at the tests step with no CPU activity. The cause was an ORPHANED ENGINE PROCESS holding a fixed test port:

    PID 47683  ./target/debug/mysql-ultralight --port 33399   started Sep 2 15:15

That is over a day old, left behind by a long-finished run. The current gate had started Sep 3 19:32 and was waiting on a port the orphan owned. Killing it let the gate proceed immediately, and it then passed clean.

Four more orphans were still running from a worktree removed hours earlier (agent-addbcf1571e462a02), so the leak is not a one-off.

This reframes the issue: the 5s timeouts blamed on 'concurrent load' are probably a test failing to bind a fixed port that a previous run never released, which concurrency makes MORE LIKELY but does not cause. Worth checking which tests use fixed ports rather than port 0, and whether the harness that spawns them reliably reaps on failure -- a test that panics before its guard runs would leak exactly like this.

Practical mitigation until then: pgrep -f 'mysql-ultralight --port' before a gate run and kill anything older than the run.

### 2026-09-04T00:22:02Z — Christopher Vachon (user)

Follow-up to the orphaned-port finding: the same session also hit a SECOND environment failure worth recording beside it. Docker's containerd metadata DB (/var/lib/containerd/io.containerd.metadata.v1.bolt/meta.db) began failing every write with EIO, after the host disk reached 100%. Symptom: containers report 'Up' in docker ps while their processes are gone, docker exec fails, docker rm fails, and no new container can start. A Docker Desktop restart cleared it.

Both failures presented as 'the gate hangs or the differential will not start' and neither is concurrency. Worth checking before blaming load:
  1. pgrep -f 'mysql-ultralight --port' for orphans older than the run
  2. df -h / -- the repo's target/ reached 28G and agent worktrees another 9.6G
  3. docker exec <any-container> true -- if it fails with an I/O error, restart Docker
