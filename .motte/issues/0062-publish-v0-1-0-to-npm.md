---
id: 62
title: Publish v0.1.0 to npm
state: Todo
parent: 8
labels: [phase-7]
created: 2026-08-21T13:06:15Z
updated: 2026-08-24T14:16:58Z
---

## Description

Everything the release needs is built and tested; nothing has been published. This is the remaining work of #0008, which was recorded only in a note and so left the epic looking finished while its whole point was outstanding.

.github/workflows/release.yml is driven by a tag. It builds macOS arm64, Linux x64 (glibc and musl) and Linux arm64, stages each binary into its platform package with packages/npm/scripts/prepare-release.js, tests the wrapper against the binary it just built, then publishes the four platform packages before the wrapper that depends on them — that order matters, because the wrapper resolves its platform package at runtime from a name built by template string.

Two things are the maintainer's to do and cannot be done from here:

- an NPM_TOKEN secret on the repository, with publish rights to @codevachon
- a deliberate version tag to fire the workflow

Worth confirming before the tag: the version in packages/npm/package.json and in each platform package agree with the tag, and a dry run of prepare-release.js stages what the workflow expects.

## Notes

### 2026-08-24T14:16:58Z — Christopher Vachon (user)

Release pre-flight for v0.1.0. One real blocker found and fixed, everything else agreed.

Blocker: the release could not have completed. The build matrix smoke-tests each binary with `mysql-ultralight --help > /dev/null`, and asking for help exited 2. Every GitHub Actions `run:` step is `bash -e`, so the step failed, and with fail-fast on the matrix the whole release aborted before the publish job ran. Nothing had noticed because nothing had ever tagged. Fixed in 95541dd: help goes to stdout and exits 0, a misuse still goes to stderr and exits 2, and crates/cli/tests/cli.rs asserts both. The same bug broke `npx mysql-ultralight --help`, which packages/npm/README.md documents.

Verified, not assumed:
- Versions all 0.1.0: wrapper, four platform packages, optionalDependencies pins, Rust workspace (every crate is version.workspace). No README or CHANGELOG snippet pins a version.
- The workflow's four matrix targets are exactly the four rustTarget values the platform packages declare: aarch64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, x86_64-unknown-linux-musl.
- The four package names are the same string in three places: optionalDependencies, the PLATFORMS map in src/binary.js, and each platform manifest's name. Directory keys match the binary.js keys too.
- prepare-release.js dry run against real binaries laid out the way download-artifact produces them (binaries/<target>/mysql-ultralight, exec bit stripped): staged all four, chmod 755, exit 0, no manifest churn.
- npm pack of all five, then installed the wrapper plus the darwin tarball into a clean project: require.resolve found the binary inside the platform package and start() booted a server and returned a URL. That exercises the runtime resolution the publish order exists to protect.
- Publish order in release.yml is correct: the four platform packages, then the wrapper.

Not fixed, reported instead: the CLI has no --version flag at all (--version hits the unknown-arg branch). The version it does report, over COM_STATISTICS, is CARGO_PKG_VERSION and agrees. Adding the flag is a feature, so it was left for a decision.

Also noted, out of scope: crates/conformance/src/main.rs has the identical --help-exits-2 shape. Nothing in CI or check.sh runs that binary, so it cannot break a build today.

Still the maintainer's to do, and deliberately not done here: the NPM_TOKEN secret, and the tag. Nothing was published, tagged, or pushed.
