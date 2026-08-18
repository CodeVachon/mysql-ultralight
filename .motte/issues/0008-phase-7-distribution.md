---
id: 8
title: Phase 7 — Distribution
state: Todo
labels: [epic, phase-7]
blockedBy: [3]
created: 2026-08-18T14:10:54Z
updated: 2026-08-18T14:11:32Z
---

## Description

Three delivery channels, all from one Rust binary: an npm package shipping prebuilt per-platform binaries for jest setup to spawn, a standalone CLI for CI and local dev, and unix socket support alongside TCP to avoid port races under parallel test workers.

Platforms: macOS arm64, Linux x64 (glibc and musl), Linux arm64. Windows and macOS x64 explicitly out of scope for v1.
