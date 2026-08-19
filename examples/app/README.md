# The example app

One Prisma schema, one migration history, one set of controllers. The runner
examples next door — [`node-test`](../node-test), [`jest`](../jest),
[`vitest`](../vitest) — contain nothing but tests, and all three test this.

That split is the point. Comparing three test runners is only useful if the
thing under test is identical, and a schema copied three times is a schema that
drifts three ways.

## What is here

| | |
|---|---|
| `prisma/schema.prisma` | The schema, chosen for coverage rather than realism |
| `prisma/migrations/` | Four migrations: create, seed, an `ALTER`, a data migration |
| `src/controllers/` | The application logic the tests exercise |
| `src/testing.ts` | How a test gets a database |

The schema covers what a real application leans on: one-to-one, one-to-many and
implicit many-to-many relations; enums, JSON, `Decimal`, `LongText`, and
`DateTime` with `now()` and `@updatedAt`; cuid, uuid and autoincrement ids; a
compound unique, a secondary index, and cascading deletes.

The controllers take their client as an argument rather than reaching for a
module-level one. That is what lets a test hand each file a database of its
own, and it is the only thing an application has to do differently to make this
work.

## How a test gets a database

```ts
import { startDatabase } from '@mysql-ultralight-example/app';

const { db, stop } = await startDatabase();   // ~1ms
```

Two functions, and the split between them is what makes a database per test
file affordable:

- **`bake()`** applies the migrations and snapshots the result. Expensive, and
  it runs **once** — from `npm run bake`, which every runner's `pretest`
  invokes, in one process before any worker exists.
- **`startDatabase()`** only ever loads that image. It is what tests call. It
  refuses to bake on the spot, because a test hook is the wrong place to
  discover the migrations have not run — and a multi-second migration inside a
  hook with a five-second timeout is a failure that reads like a flake.

`connect()` and `startEmpty()` are there for the two other shapes: connecting to
a database someone else started, and starting one with no schema at all, which
only the migration tests want.

## Commands

```sh
npm run build     # prisma generate, then tsc
npm run bake      # build, then apply the migrations once into .cache/
npm run typecheck
```

The image is keyed by a hash of the migrations *and* the engine binary, so it
is reused until either changes and rebuilt when they do.
