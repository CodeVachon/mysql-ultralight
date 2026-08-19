---
id: 8
title: Phase 7 — Distribution
state: Todo
labels: [epic, phase-7]
blockedBy: [3]
created: 2026-08-18T14:10:54Z
updated: 2026-08-19T02:20:07Z
---

## Description

Three delivery channels, all from one Rust binary: an npm package shipping prebuilt per-platform binaries for jest setup to spawn, a standalone CLI for CI and local dev, and unix socket support alongside TCP to avoid port races under parallel test workers.

Platforms: macOS arm64, Linux x64 (glibc and musl), Linux arm64. Windows and macOS x64 explicitly out of scope for v1.

## Notes

### 2026-08-19T02:20:07Z — Christopher Vachon (user)

The npm package exists: @codevachon/mysql-ultralight, with a programmatic API (start({ image }) returning a connection URL and a stop handle), a CLI shim, four platform packages as optional dependencies, and 20 tests including a full bake-then-boot cycle against the real binary.

Release is driven by a tag: .github/workflows/release.yml builds the four targets, stages each binary into its platform package with packages/npm/scripts/prepare-release.js, tests the wrapper against the binary it just built, then publishes the platform packages before the wrapper that depends on them. Nothing is published yet - that needs an NPM_TOKEN secret and a deliberate tag.

Being real JavaScript, the package is now covered by Fallow in scripts/check.sh, for that subtree only, as CLAUDE.md always said it would be.
