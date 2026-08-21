---
id: 62
title: Publish v0.1.0 to npm
state: Todo
parent: 8
labels: [phase-7]
created: 2026-08-21T13:06:15Z
updated: 2026-08-21T13:06:15Z
---

## Description

Everything the release needs is built and tested; nothing has been published. This is the remaining work of #0008, which was recorded only in a note and so left the epic looking finished while its whole point was outstanding.

.github/workflows/release.yml is driven by a tag. It builds macOS arm64, Linux x64 (glibc and musl) and Linux arm64, stages each binary into its platform package with packages/npm/scripts/prepare-release.js, tests the wrapper against the binary it just built, then publishes the four platform packages before the wrapper that depends on them — that order matters, because the wrapper resolves its platform package at runtime from a name built by template string.

Two things are the maintainer's to do and cannot be done from here:

- an NPM_TOKEN secret on the repository, with publish rights to @codevachon
- a deliberate version tag to fire the workflow

Worth confirming before the tag: the version in packages/npm/package.json and in each platform package agree with the tag, and a dry run of prepare-release.js stages what the workflow expects.
