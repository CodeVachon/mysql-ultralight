---
id: 81
title: differential.sh can compare against a stale engine and report green
state: Todo
parent: 9
labels: [testing]
created: 2026-08-24T18:02:26Z
updated: 2026-08-24T18:02:26Z
---

## Description

Found during #0053, by a mutation run that reported PASSED WITH THE MUTATION IN PLACE when the mutation was in fact live.

A leftover mysql-ultralight process on port 33307 from an earlier debugging step meant the harness connected to a previous build. scripts/differential.sh's readiness check only tests that something accepts a connection, and check_identities verifies the peer is *a* mysql-ultralight -- not that it is *this* build. So the run compared MySQL against a stale binary and reported 0 divergences.

This is the harness's worst failure mode. Its whole value is that a green run means the engine answers as MySQL answers; a green run that means an old engine did is a false pass, and the mutation check -- the thing that proves a test bites -- is exactly where it does the most damage, because there a false green reads as 'this test does not bite' or, worse, as 'the mutation was harmless'.

Fix by making the identity check bind to the build: have the engine report something that changes per build (a hash of the binary, or the build's own version plus a start-up nonce the harness reads back) and refuse to run against a listener that does not match the one the script just started. Refusing to reuse a listener the script did not itself start would also do it, at the cost of --keep.

The same hole makes --keep unsafe in general: it exists to reuse a running MySQL between runs, and nothing distinguishes reusing MySQL from reusing a stale engine.
