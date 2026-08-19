# A worked Prisma application

A small application run against `mysql-ultralight` the way a real one would be:
its own schema, its own migration history, its own seed data, and tests that go
through Prisma Client rather than around it.

It exists for two reasons. It is the documentation to copy — the bake-and-boot
workflow here is the one the top-level README describes — and it is the only
thing in the gate that exercises the npm package, the wire protocol and Prisma
Client together. The layers are tested separately elsewhere; the seam between
them is tested here.

## Run it

```sh
npm install
npm test
```

The tests build nothing. They expect the engine binary to exist already, which
`cargo build --release` at the repository root provides — `npm install` links
`@codevachon/mysql-ultralight` to `packages/npm`, and that resolves the local
build. `scripts/check.sh` does both in the right order.

## What it covers

The schema is chosen for coverage rather than realism. One-to-one, one-to-many
and implicit many-to-many relations; enums, JSON, `Decimal`, `LongText`, and
`DateTime` with `now()` and `@updatedAt`; cuid, uuid and autoincrement ids; a
compound unique, a secondary index, and cascading deletes.

The migration history is a history rather than a single `CREATE`: four
migrations, including seed data, an `ALTER` that adds a column and widens
another, and a data migration that backfills the new column from the old one.

`test/migrate.test.js` asserts the claim the project rests on — that an image
baked from the history is the same database the history produces. It compares
`information_schema` for both, down to types, nullability and keys, and checks
that `prisma migrate deploy` against the image reports nothing pending.

`test/client.test.js` runs Prisma Client: reads with filters, ordering and
pagination, relations in both directions, aggregates and `groupBy`, a nested
create, `connectOrCreate`, upserts, a rolled-back transaction, the constraints
the schema declares, and raw queries.

One of those raw queries reads columns **by name**. That is not decoration: the
engine once declared no columns at prepare time, so every raw result came back
positionally and a lookup by name found nothing. Every unit test passed. Prisma
Client hid it by mapping on position from its own schema, and only a raw query
through a real driver could see it.

## The workflow it demonstrates

```sh
npm run bake     # migrate once into .cache/<hash>.mus
```

The image is keyed by a hash of `prisma/migrations`, so it is reused until the
migrations change and rebuilt when they do. Keying it by anything looser
eventually boots a suite against a schema its migrations no longer describe,
and those failures land nowhere near the cause.

Each test then starts its own server from that image, which costs about a
millisecond — which is what makes a fresh database per test affordable, and the
whole point of the project.

## What building this found

Three engine bugs, none of which any unit test could have reached, because each
only appears when a real client asks a real question:

- **`information_schema` reported numbers as text.** Prisma's schema describer
  reads `seq_in_index` as an integer and unwraps it, so `prisma db pull`,
  `migrate dev` and `migrate diff` all crashed outright. Introspection works now.
- **A binary string was true whenever it was non-empty.** `BINARY '0'` was
  therefore true, and Prisma's foreign-key query — which compares
  `BINARY a = BINARY b` — matched every pair. A four-row answer came back as
  sixty-four, silently.
- **`NOW(3)` ignored its precision**, so a `DATETIME(3)` column filled by it
  held whole seconds and two rows written in the same second compared equal.

## Prisma version

Pinned to 5.4.2, matching `apps/api` in the Appello monorepo. The point of this
example is to catch what that application would hit, so it should ask the engine
the same questions in the same way. Bump it when the monorepo bumps.
