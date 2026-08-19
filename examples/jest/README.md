# Jest

Testing [the example app](../app) with Jest, two ways. The app owns the
schema, the migrations and the controllers; this package is nothing but tests
and the wiring that gives them a database.

```sh
npm install
npm test          # runs both models
```

Both models are here because they are a migration path rather than
alternatives: the first is the smallest change to a suite that already talks to
MySQL, and the second is what the first cannot afford.

TypeScript with `ts-jest`, because that is what a real suite tends to be.

## The drop-in: one database for the run

`jest.config.shared.js` → `test/support/global-setup.ts`

A suite backed by a real MySQL usually has a `globalSetup` that brings a
database up, migrates it, and puts its URL in `DATABASE_URL`. This replaces the
inside of that function and nothing else:

```ts
export default async function globalSetup() {
    const database = await startDatabase();          // loads a baked image
    process.env.DATABASE_URL = database.server.url;  // workers inherit it
    globalThis.__DATABASE__ = database;              // teardown runs here too
}
```

The variable is set before Jest forks its workers, so every one of them
inherits it.

What changes is the cost: replaying a migration history takes seconds, loading
an image takes about a millisecond. What does *not* change is that every test
file shares one database — `test/shared/leaks.test.ts` shows a file seeing
another file's rows, and why nothing in that model can assert an exact count.

## The upgrade: one database per test file

`jest.config.isolated.js` → `test/support/per-file.ts`

```ts
beforeAll(async () => {
    starting = startDatabase();
    database = await starting;
});
```

Registered through `setupFilesAfterEnv`, which Jest runs once per test file
inside its worker — so `beforeAll` means "before this file".

`test/isolated/authors.test.ts` and `test/isolated/posts.test.ts` both register
`claimed@example.com`, a unique column. Under a shared database one of them
fails depending on which ran first. Here both pass, in any order, in parallel.

Isolation is per **file**, not per test: the tests inside one file run in order
against one database, and `authors.test.ts` says so where it matters — one of
its tests deletes the seeded author, so the ones after it set up their own.

## Two things worth copying carefully

- **The in-flight start.** `per-file.ts` keeps the promise from `startDatabase()`
  as well as its result. If `beforeAll` ever times out, the hook is abandoned
  but the server it asked for still arrives; without the promise, `afterAll`
  sees `undefined` and the server is detached, so nothing ever stops it.
- **Let the port default to 0.** The OS picks a free one, so parallel workers
  cannot race for a port — which is what makes the per-file model work at all.
