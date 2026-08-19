---
id: 34
title: A worked Prisma example the gate can run
state: Todo
parent: 9
labels: [compat, prisma, testing]
created: 2026-08-19T13:22:10Z
updated: 2026-08-19T14:01:13Z
---

## Description

Everything proving this engine works against Prisma lives outside the repository. The conformance target is a private monorepo at ~/Repositories/Appello/application-mono-repo, and the checks that matter most — 604 migrations applying, a client round-tripping real models — are run by hand against a checkout CI does not have. Nothing in scripts/check.sh exercises the npm package, the wire protocol and Prisma Client together.

A small Prisma application inside the repository fixes that, and doubles as the documentation people copy.

## What it is

A self-contained app at examples/prisma-app/, with its own schema, migration history, seed data and tests. Small enough to stay fast in the gate, broad enough that a regression in any layer shows up.

Prisma is pinned to 5.4.2, the version apps/api runs in the Appello monorepo. The point of this example is to catch what that application would hit, so it should ask the engine the same questions the same way. Bump it when the monorepo bumps.

The schema should cover the surface a real application leans on rather than a toy: one-to-one, one-to-many and implicit many-to-many relations; enums; JSON; DateTime with @updatedAt; Decimal; cuid, uuid and autoincrement defaults; compound unique keys; indexes; and cascading deletes.

The migration history has to be a history, not a single CREATE: several migrations including an ALTER that changes a column type and a data migration that rewrites rows, so prisma migrate deploy is exercised the way it is in practice.

## What it tests

- prisma migrate deploy from empty applies the whole history.
- The same schema booted from a state image is indistinguishable from the migrated one — compare information_schema for both, and assert migrate deploy against the image reports nothing pending.
- Prisma Client through the npm package: create, nested writes, findMany with filters and ordering and pagination, aggregations and groupBy, transactions including a rollback, upserts, and $queryRaw reading columns by name.
- The bake-and-boot workflow the README describes, keyed by a hash of the migrations directory, so the documented path is the tested path.

## Why it earns its place in the gate

The wire protocol is tested at packet level and the engine at SQL level, but the seam where a real client's expectations meet both is only covered by hand today. The prepared-statement column-name bug is the case in point: every unit test passed while every raw query came back with positional names, because nothing in the gate read a result by column name through a real driver.

## Done when

- node --test in the example passes against a freshly built binary.
- scripts/check.sh runs it unconditionally, and therefore so do CI and the pre-commit hook.
- README points at it as the worked example.
- The baked image is produced by the test run, not committed. node_modules is not committed either, so CI caches it through actions/setup-node rather than paying the cold install every run.

## What it is

A self-contained app, recommended at examples/prisma-app/, with its own schema, migration history, seed data and tests. Small enough to stay fast in the gate, broad enough that a regression in any layer shows up.

The schema should cover the surface a real application leans on rather than a toy: one-to-one, one-to-many and implicit many-to-many relations; enums; JSON; DateTime with @updatedAt; Decimal; cuid, uuid and autoincrement defaults; compound unique keys; indexes; and cascading deletes.

The migration history has to be a history, not a single CREATE: several migrations including an ALTER that changes a column type and a data migration that rewrites rows, so prisma migrate deploy is exercised the way it is in practice.

## What it tests

- `prisma migrate deploy` from empty applies the whole history.
- The same schema booted from a state image is indistinguishable from the migrated one — compare information_schema for both, and assert migrate deploy against the image reports nothing pending.
- Prisma Client through the npm package: create, nested writes, findMany with filters and ordering and pagination, aggregations and groupBy, transactions including a rollback, upserts, and $queryRaw reading columns by name.
- The bake-and-boot workflow the README describes, keyed by a hash of the migrations directory, so the documented path is the tested path.

## Why it earns its place in the gate

The wire protocol is tested at packet level and the engine at SQL level, but the seam where a real client's expectations meet both is only covered by hand today. The prepared-statement column-name bug is the case in point: every unit test passed while every raw query came back with positional names, because nothing in the gate read a result by column name through a real driver.

## Done when

- `node --test` in the example passes against a freshly built binary.
- scripts/check.sh runs it, and therefore so does CI and the pre-commit hook.
- README points at it as the worked example.
- The baked image is produced by the test run, not committed.

## Decisions still open

- Location: examples/prisma-app/ keeps it clearly a sample; packages/example/ would make it an npm workspace sibling. The first reads better to a visitor.
- Whether Prisma is pinned exactly. A floating version turns an upstream release into a red gate on an unrelated commit; a pinned one stops the example noticing that Prisma changed what it asks for. Pinning with a scheduled job that bumps it is the usual compromise.
- Whether the gate runs it always or only under --fast=0. It needs npm install of Prisma, which is not free.

## Notes

### 2026-08-19T14:01:13Z — Christopher Vachon (user)

Three decisions settled, one of them by measurement rather than argument.

Location: examples/prisma-app/.

Prisma version: pinned to 5.4.2, matching apps/api in the Appello monorepo, and bumped when that bumps.

Whether the gate always runs it: yes. I had worried the npm install made it too expensive to run unconditionally; measured on this machine that was wrong. Cold install with an empty npm cache is 4.19s and 54 MB, a warm install is 0.96s, npm ci from a lockfile is 0.54s, prisma generate is 0.41s and prisma migrate deploy of a one-migration history is 0.68s. Once node_modules exists a gate run pays about 1.1s, which is noise beside the Rust build. Only CI pays the cold path, and only until setup-node's cache is pointed at the example.
