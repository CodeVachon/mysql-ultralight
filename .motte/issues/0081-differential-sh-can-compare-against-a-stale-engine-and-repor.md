---
id: 81
title: differential.sh can compare against a stale engine and report green
state: Done
parent: 9
labels: [testing]
created: 2026-08-24T18:02:26Z
updated: 2026-08-24T18:45:15Z
---

## Description

Found during #0053, by a mutation run that reported PASSED WITH THE MUTATION IN PLACE when the mutation was in fact live.

A leftover mysql-ultralight process on port 33307 from an earlier debugging step meant the harness connected to a previous build. scripts/differential.sh's readiness check only tests that something accepts a connection, and check_identities verifies the peer is *a* mysql-ultralight -- not that it is *this* build. So the run compared MySQL against a stale binary and reported 0 divergences.

This is the harness's worst failure mode. Its whole value is that a green run means the engine answers as MySQL answers; a green run that means an old engine did is a false pass, and the mutation check -- the thing that proves a test bites -- is exactly where it does the most damage, because there a false green reads as 'this test does not bite' or, worse, as 'the mutation was harmless'.

Fix by making the identity check bind to the build: have the engine report something that changes per build (a hash of the binary, or the build's own version plus a start-up nonce the harness reads back) and refuse to run against a listener that does not match the one the script just started. Refusing to reuse a listener the script did not itself start would also do it, at the cost of --keep.

The same hole makes --keep unsafe in general: it exists to reuse a running MySQL between runs, and nothing distinguishes reusing MySQL from reusing a stale engine.

## Notes

### 2026-08-24T18:45:15Z — Christopher Vachon (user)

Done in 01c5493. Three layers, each failing the run rather than warning: the engine mints a per-process instance id and prints it on the ready line; scripts/differential.sh waits for the ready line of the process it started rather than polling the port, so a leftover holder surfaces as the engine's own AddrInUse; and the harness requires --engine-instance and refuses a peer reporting a different id or none.

The alternative -- refuse any listener the script did not start -- gives the same guarantee with less proof and nowhere to test it. Identifying the peer over the connection the comparison actually runs on is testable, and covers a hand-run binary the script never sees.

--keep now keeps MySQL and only MySQL. It exists because MySQL costs seconds to start; the engine costs 0.6 ms, so reusing it saved nothing and gave up the only guarantee the harness makes.

Reproduced the original bug and it now exits 2 in all three shapes: a stale listener holding the port, a harness pointed at a leftover with another id, and a build predating the variable (1193, quoted back rather than asserted as a cause).

Review found the gate did not cover the ready line's new field -- a mutation blanking it passed every check -- so packages/npm/test/start.test.js now asserts its shape against the real binary. It failed against a stale target/release build the moment it was written, which is the same class of bug this issue is about.
