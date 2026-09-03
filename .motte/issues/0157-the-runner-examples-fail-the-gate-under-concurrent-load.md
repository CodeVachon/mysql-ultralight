---
id: 157
title: The runner examples fail the gate under concurrent load
state: Todo
parent: 9
labels: [testing, app-priority-none]
created: 2026-09-03T02:40:14Z
updated: 2026-09-03T14:20:30Z
---

## Description

Observed repeatedly while running several agents against one checkout, and it has now cost several gate cycles.

scripts/check.sh's vitest and jest stages fail with 5000 ms hook or test timeouts whenever another cargo build, another example suite, or a mutate.sh loop is running on the same machine. The same suites take 350-650 ms when the box is quiet -- measured: examples/vitest standalone is 350 ms for 3 tests, against a 5 s timeout it exceeded under load.

The failure says nothing about the cause. It names a test that is not slow and gives no hint that the machine was busy, so the reader re-runs it, sees it pass, and learns to distrust the gate -- which is the same damage #0108's temp-directory collision did, from a different direction. A gate that fails for reasons unrelated to the change is worse than a slow one.

Options worth measuring before choosing: raise the runner timeouts (cheapest, but hides a genuinely slow suite); run the example suites with a concurrency limit; or have check.sh serialise its npm stages. Note CI runs on a quiet box, so this is a local-development problem specifically -- which is where the gate is run most often.

Related: #0108 (image tests collided on fixed temp paths, now fixed) is the same class -- a test failing because of something other than the code under test.
