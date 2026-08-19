# node:test

Testing [the example app](../app) with Node's built-in runner, and the place
the engine-level claims are checked.

```sh
npm install
npm test
```

No runner to install and no transform: the app ships compiled JavaScript, so
this package has one dependency and no configuration at all.

## What it covers

`test/client.test.js` exercises the app's controllers through the whole stack —
the npm package, the wire protocol, the SQL and Prisma Client. Among them is a
raw query that reads columns **by name**: the engine once declared no columns at
prepare time, so every raw result came back positionally and a lookup by name
found nothing. Every unit test passed. Only a real driver could see it.

`test/migrate.test.js` checks the claim the project rests on:

- the history applies from empty, including the `ALTER` and the data migration;
- an image baked from it describes the same database — compared through
  `information_schema` down to types, defaults, indexes and foreign keys;
- `prisma migrate deploy` against a server booted from the image reports
  nothing pending;
- `prisma migrate diff` reports no drift between the migrations and the
  datamodel, so the hand-written SQL is not a fiction;
- booting from the image stays under 100ms, which is a guard rather than a
  benchmark: if it ever costs what replaying migrations costs, the reason this
  project exists is gone.

Those last two run Prisma's schema engine against this engine, so they also
prove introspection works — `migrate diff` needs a shadow database, and the one
it gets is `mysql-ultralight`.
