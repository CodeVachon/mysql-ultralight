---
id: 76
title: "Cut the v0.1.0 release: token, push, tag"
state: Todo
parent: 62
labels: [phase-7, release, maintainer-only]
created: 2026-08-24T14:20:53Z
updated: 2026-08-24T14:21:51Z
---

## Description

The maintainer-only remainder of #0062. Every part of the release that could be verified from a checkout has been, and a bug that would have aborted the workflow was found and fixed (95541dd); what is left needs credentials and a deliberate, irreversible act, so it is recorded here rather than left implicit in a closed issue.

Two commits are local and unpushed as of 2026-08-24:

- 95541dd  Let --help succeed, so the release can get past its smoke test
- 7eae383  Record the v0.1.0 pre-flight on #0062

Do these in order:

1. Add an NPM_TOKEN repository secret with publish rights to the @codevachon scope. Without it the publish job fails after the four targets have already built.

2. Review and push both commits. The tag must point at a commit containing 95541dd. release.yml smoke-tests every freshly built binary with `mysql-ultralight --help > /dev/null`; before that fix --help exited 2, and because each run: step is bash -e and the build matrix sets fail-fast: true, the first target to finish would have killed the release before the publish job existed. Tagging 40ce26b or anything earlier still aborts there.

3. Optional but recommended: fire the workflow by workflow_dispatch with version 0.1.0 and dry-run true. It builds, stages, tests and packs, and skips publishing. This is the only way to find out whether the two cross-compiled targets build — aarch64-unknown-linux-gnu and x86_64-unknown-linux-musl go through cross and have never been executed by anyone. There is no Cross.toml, though the CLI's dependency tree pulls in no -sys crates and no build.rs, so no C toolchain should be involved.

4. Tag v0.1.0 and push the tag. prepare-release.js strips the leading v; on a tag push the version resolves from ${GITHUB_REF_NAME#v}, and inputs.dry-run is empty so publishing really runs. The four platform packages publish before the wrapper that depends on them, which matters because the wrapper resolves its platform package at runtime from a name built by template string.

A published version cannot be reused, so step 3 is worth the wait.

What was already confirmed, so it does not need redoing: versions agree at 0.1.0 across the wrapper, the four platform packages, the optionalDependencies pins and every Rust crate; the workflow's four targets equal the four declared rustTarget values; the names the wrapper builds at runtime equal the published package names, compared as strings; a prepare-release.js dry run against real binaries in the download-artifact layout staged all four with no manifest churn; and a pack-and-install into a clean project resolved the platform binary and booted a server.

Out of scope, noted while looking: the CLI has no --version flag at all, and crates/conformance/src/main.rs:108 has the same --help-exits-2 shape, harmless today because nothing in CI or check.sh runs that binary.
