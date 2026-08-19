# Vitest

Testing [the example app](../app) with Vitest, two ways. The app owns the
schema, the migrations and the controllers; this package is nothing but tests
and the wiring that gives them a database.

```sh
npm install
npm test          # runs both models
```

Both models are here because they are a migration path rather than
alternatives: the first is the smallest change to a suite that already talks to
MySQL, and the second is what the first cannot afford.

TypeScript throughout, with no transform to configure: Vitest reads it natively.

## The drop-in: one database for the run

`vitest.shared.config.ts` → `test/support/global-setup.ts`

Vitest's `globalSetup` returns its own teardown, so there is no second file and
no global to stash a handle in — the database is closed over:

```ts
export default async function setup() {
    const database = await startDatabase();
    process.env.DATABASE_URL = database.server.url;
    return async () => {
        await database.stop();
    };
}
```

Vitest runs test files in **forked child processes** by default, and forks them
after this has run, so they inherit the variable. It works under
`pool: 'threads'` too, where the workers share this process and therefore this
environment.

Every test file shares that one database. `test/shared/leaks.test.ts` shows a
file seeing another file's rows, and why nothing in that model can assert an
exact count.

## The upgrade: one database per test file

`vitest.isolated.config.ts` → `test/support/per-file.ts`

`setupFiles` runs once per test file, so the hooks it registers are per file.
There is no `globalSetup` at all, because nothing is shared.

`test/isolated/authors.test.ts` and `test/isolated/posts.test.ts` both register
`claimed@example.com`, a unique column. Under a shared database one of them
fails with `Unique constraint failed on the constraint: Author_email_key` —
verified by pointing them at one. Here both pass, in any order, in parallel.

Isolation is per **file**, not per test: the tests inside one file run in order
against one database.

## Differences from the Jest example

Only the wiring; the app and the engine side are identical.

| | Jest | Vitest |
|---|---|---|
| Per-run hook | `globalSetup` + `globalTeardown`, two files | `globalSetup` returning a teardown |
| Per-file hook | `setupFilesAfterEnv` | `setupFiles` |
| Workers | child processes | forked child processes by default |
| TypeScript | `ts-jest` | native |

A leaked server holds neither runner open, because the child is detached — so
`per-file.ts` stops it explicitly, including when the start is still in flight.
