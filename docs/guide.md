# Using it in a test suite

This page shows how to put mysql-ultralight under a test suite that currently talks to
MySQL 8. It goes from installing it, through connecting a client, to giving every test
file a database of its own.

Every code sample on this page is a shortened copy of code that runs in the gate. The
full versions are in [`examples/`](https://github.com/CodeVachon/mysql-ultralight/tree/main/examples),
and the links below point at them.

> If you are an AI agent setting this up for someone, [For AI agents](for-agents.md)
> has the same material as rules and recipes, without the narrative.

## What you get, and what you give up

You get a server that speaks the MySQL wire protocol, accepts MySQL 8 SQL, and starts in
milliseconds rather than the seconds a MySQL container takes. Any MySQL client can connect
to it: Prisma, `mysql2`, the `mysql` CLI. If you bake your migrations into a *state image*
once, each test process loads the whole schema from it instead of replaying
`migrate deploy`. [The working prototype](prototype.md) and
[What real MySQL costs](baseline-mysql.md) have the measured numbers, with their dates.

What you give up:

- **Durability.** Everything is in memory and is gone when the process exits. That is
  the design, not a gap.
- **Isolation between concurrent connections.** Two connections share one catalog, so
  one can see the other's uncommitted writes. The model this engine is built for is one
  database per test file, and in that model it does not come up. See
  [What it does not do](#what-it-does-not-do).
- **Everything MySQL has not been asked for yet.** SQL the engine does not support fails
  with error `1235` (`ER_NOT_SUPPORTED_YET`) and a message naming the feature. It never
  answers with a guess.

## Install

```sh
npm install --save-dev @codevachon/mysql-ultralight
```

Prebuilt binaries come as optional dependencies for **macOS arm64** and **Linux x64/arm64**
(glibc and musl). Windows and Intel macOS are not supported.

> **Before the first npm release**, install it from a checkout instead:
>
> ```sh
> git clone https://github.com/CodeVachon/mysql-ultralight /path/to/mysql-ultralight
> cargo build --release --manifest-path /path/to/mysql-ultralight/Cargo.toml
> cd /your/project && npm install --save-dev /path/to/mysql-ultralight/packages/npm
> ```
>
> npm links a local path rather than copying it, so the package finds
> `target/release/mysql-ultralight` by itself. On any other setup, set
> `MYSQL_ULTRALIGHT_BINARY` to the binary's path.

The package needs Node 18 or newer. The examples use Node 20.11.

## Start a server and connect to it

```js
import { start } from '@codevachon/mysql-ultralight';

const db = await start();
console.log(db.url); // mysql://root@127.0.0.1:53412/test

// ... connect, run tests ...

await db.stop();
```

`start()` resolves once the server is accepting connections, so you don't need to poll
or sleep. The port defaults to `0`, which means the OS picks a free one. That is why
parallel test workers can each start a server without racing for a port. Don't pin a
port in tests.

If you don't pass `password`, the server accepts any username and password. The URL uses
`root`.

### Prisma

Prisma reads the URL as it is:

```js
process.env.DATABASE_URL = db.url;
// or
const prisma = new PrismaClient({ datasources: { db: { url: db.url } } });
```

### mysql2 and other drivers

Pass the fields separately. With `socket: true`, `db.url` carries the socket as a
`?socket=` parameter. Prisma understands that parameter, but most drivers don't.

```js
import mysql from 'mysql2/promise';

const conn = await mysql.createConnection(
    db.socketPath
        ? { socketPath: db.socketPath, user: 'root', database: db.database }
        : { host: '127.0.0.1', port: db.port, user: 'root', database: db.database }
);
```

### The `mysql` CLI

```sh
mysql -h 127.0.0.1 -P 53412 -u root test
```

## Skip the migrations: state images

This is the main reason the project exists. Replaying a migration history takes seconds.
Loading the result of that replay takes milliseconds, even for a schema of hundreds of
tables. [The state image](prototype.md#the-state-image) has the measurements.

You do this in two steps.

**Bake, once per change to the migrations.** Start an empty server, run your real
migrations against it, and snapshot the result. The snapshot is an ordinary `SELECT`, so
any client can take it:

```js
import { start } from '@codevachon/mysql-ultralight';
import { PrismaClient } from '@prisma/client';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const image = path.resolve('.cache/schema.mus');
// The server writes the file but does not create its directory.
fs.mkdirSync(path.dirname(image), { recursive: true });

const server = await start({ database: 'app' });
try {
    execFileSync('npx', ['--no-install', 'prisma', 'migrate', 'deploy'], {
        env: { ...process.env, DATABASE_URL: server.url },
        stdio: 'inherit'
    });
    const prisma = new PrismaClient({ datasources: { db: { url: server.url } } });
    try {
        await prisma.$queryRawUnsafe(`SELECT ultralight_snapshot('${image}')`);
    } finally {
        await prisma.$disconnect();
    }
} finally {
    await server.stop();
}
```

**Boot from it, in every test process.**

```js
const db = await start({ image, database: 'app' });
```

Pass the same `database` name you baked with. With a different name, the server loads the
image *and* creates a new, empty database under that name, and `db.url` points at the
empty one. Nothing fails until the first query finds no tables.

A server booted from an image can't be told apart from one that ran the migrations. The
image carries tables, seed rows, indexes, foreign keys, generated columns, `CHECK`
constraints and auto-increment counters. `prisma migrate deploy` against it reports
"No pending migrations to apply".

Four rules keep images honest:

1. **Use an absolute path.** The *server* process writes the file, and it resolves a
   relative path against its own working directory.
2. **Name the image after a hash of the migrations and the engine binary.** A stale
   image is a schema your tests are not really running against. Only a change to the
   image *format* is refused at load. An engine upgrade that keeps the format loads an
   old image without complaint, which is why the binary belongs in the hash. See
   [`fingerprint()` in `examples/app/src/testing.ts`](https://github.com/CodeVachon/mysql-ultralight/blob/main/examples/app/src/testing.ts)
   for a version that does both.
3. **Bake before the runner starts, not inside a test hook.** Run it from a `pretest`
   script, in one process, before any worker exists. If you bake in a hook, every worker
   bakes on a cold cache, and a multi-second migration ends up inside a hook with a
   five-second timeout. That failure looks like a flake.
4. **Make tests refuse to run without an image** instead of baking on the spot. When the
   migrations have not run, a clear error is better than a slow run.

`examples/app` follows all four: `npm run bake` writes `.cache/<hash>.mus`, and every
runner's `pretest` calls it.

### Resetting a running server

```sql
SELECT ultralight_restore('/abs/path/schema.mus');
```

This replaces the whole catalog of a *running* server with the image. Use it when one
process should reset its own database between test files without starting a new server.
Don't use it on a database that other test files are using at the same moment: it resets
their data as well.

## Pick a model: shared or per file

There are two ways to wire this in. They are steps on a migration path, not competing
choices.

| | Drop-in (shared) | Per-file isolation |
|---|---|---|
| Databases | One for the whole run | One per test file |
| Change to an existing suite | Smallest: replace the inside of `globalSetup` | Move setup into a per-file hook |
| Tests can assert exact row counts | No, other files are writing too | Yes |
| Unique values must be unique across files | Yes | No |
| Affordable on real MySQL | Yes | No, that is why this exists |

The examples demonstrate the difference instead of only describing it. Two test files
register the same unique email. Under the shared model one of them fails, depending on
which ran first. Under isolation both pass, in parallel, in any order.

Per-file isolation means per **file**, not per test. The tests inside one file run in
order against one database.

### Jest

Drop-in: `globalSetup` runs before Jest forks its workers, so the workers inherit
`DATABASE_URL`.

```ts
// global-setup.ts, named by `globalSetup` in jest.config.js
export default async function globalSetup() {
    const db = await start({ image, database: 'app' });
    process.env.DATABASE_URL = db.url;
    globalThis.__DB__ = db; // globalTeardown runs in this same process
}
```

```ts
// global-teardown.ts, named by `globalTeardown`
export default async function globalTeardown() {
    await globalThis.__DB__?.stop();
}
```

Per file: register the hooks through `setupFilesAfterEnv`. Jest runs that file once per
test file, so `beforeAll` means "before this file".

```ts
let server: Server | undefined;
let starting: Promise<Server> | undefined;

beforeAll(async () => {
    starting = start({ image, database: 'app' });
    server = await starting;
});

afterAll(async () => {
    const started = server ?? (await starting?.catch(() => undefined));
    await started?.stop();
});
```

Keep the `starting` promise as well as its result. If `beforeAll` times out, Jest
abandons the hook, but the server it asked for still starts. Without the promise,
`afterAll` sees `undefined`, nothing stops that server, and it outlives the run.

Full version: [`examples/jest`](https://github.com/CodeVachon/mysql-ultralight/tree/main/examples/jest).

### Vitest

Drop-in: Vitest's `globalSetup` returns its own teardown function.

```ts
export default async function setup() {
    const db = await start({ image, database: 'app' });
    process.env.DATABASE_URL = db.url;
    return () => db.stop();
}
```

Per file: put the same `beforeAll`/`afterAll` pair as in the Jest example in a
`setupFiles` entry, and import `beforeAll` and `afterAll` from `vitest`.

Full version: [`examples/vitest`](https://github.com/CodeVachon/mysql-ultralight/tree/main/examples/vitest).

### node:test

There is no configuration. Start and stop the server in the test file:

```js
const { before, after } = require('node:test');

let db;
before(async () => { db = await start({ image, database: 'app' }); });
after(async () => { await db?.stop(); });
```

Full version: [`examples/node-test`](https://github.com/CodeVachon/mysql-ultralight/tree/main/examples/node-test).
It also checks that a baked image matches the migrations, down to types, defaults,
indexes and foreign keys.

### Make the application take its client as an argument

Per-file isolation only works if each test file can hand the code under test its own
client. If your controllers import a module-level `prisma`, every test file talks to
whichever database that module connected to first. The controllers in
[`examples/app/src/controllers`](https://github.com/CodeVachon/mysql-ultralight/tree/main/examples/app/src/controllers)
take the client as a parameter. That is the only change the application needed.

## API reference

### `start(options?): Promise<Server>`

| Option | Default | |
|---|---|---|
| `image` | none | Boot from a baked state image. Throws if the file does not exist |
| `database` | `test` | Database created at startup |
| `port` | `0` | TCP port. `0` picks a free one |
| `socket` | `false` | `true` for a temporary path under `/tmp`, or a path of your own (keep it under ~100 bytes) |
| `password` | none | Require this password. Without it, any credentials work |
| `serverVersion` | `8.3.0-mysql-ultralight` | Version string reported to clients |
| `trace` | `false` | Log every connection, command and result to stderr |
| `binary` | resolved | Use this binary instead of the resolved one |
| `readyTimeoutMs` | `10000` | How long to wait for the server to announce itself |

The package looks for the binary in this order: `binary`, then `$MYSQL_ULTRALIGHT_BINARY`,
then the platform package, then `target/release` or `target/debug` in a checkout.

### `Server`

| Member | |
|---|---|
| `url` | Connection URL. Uses the socket when there is one |
| `port`, `socketPath`, `database`, `password`, `pid` | What was started |
| `startupMs` | What the server reported for its own startup |
| `stop({ timeoutMs = 5000 }?)` | Sends SIGTERM, then SIGKILL after the timeout, waits for exit, and removes the socket file. Safe to call twice |
| `exited` | Resolves when the process exits |
| `child` | The underlying `ChildProcess` |

A server that is never stopped keeps the Node process that started it running, so a test
run that skips teardown hangs instead of exiting. Always `stop()` it in teardown.

### SQL functions and variables

| | |
|---|---|
| `SELECT ultralight_snapshot('/path.mus')` | Write the current state to an image |
| `SELECT ultralight_restore('/path.mus')` | Replace the running state with an image |
| `SELECT @@ultralight_instance` | A per-process id, also printed on the `ready` line. Use it to check that you reached the server you started, not a leftover process on the same port |

### The CLI

The same binary is on your PATH as `npx mysql-ultralight`:

```sh
npx mysql-ultralight --state .cache/schema.mus --port 3307 --trace
```

| Flag | |
|---|---|
| `--port <PORT>` | TCP port. `0` picks a free one (default) |
| `--socket <PATH>` | Also listen on a unix socket |
| `--database <NAME>` | Database created at startup (default `test`) |
| `--state <PATH>` | Load a baked state image |
| `--password <PASS>` | Require this password |
| `--server-version <V>` | Version string reported to clients |
| `--trace` | Log all protocol activity to stderr |

When the server is ready it prints one line to stdout, so a shell script can wait for
that line instead of polling:

```
ready port=3307 socket=- startup_ms=1.312 instance=3f1c0a9b6d2e4517
```

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `no state image at …` | The bake has not run. Run it before the test runner, for example from `pretest` |
| `the binary for <platform> is missing` | Optional dependencies were skipped (`--omit=optional`, `--no-optional`). Reinstall without that flag, or set `MYSQL_ULTRALIGHT_BINARY` |
| `no prebuilt binary for <platform>` | Windows or Intel macOS. Build with `cargo build --release` and set `MYSQL_ULTRALIGHT_BINARY` |
| `exited with code … before it was ready` | The reason is in the error, copied from the server's stderr. Usually a pinned port that is already in use, or an image in an older format, which needs a rebake |
| `did not start within 10000ms` | Usually a very slow CI machine. Raise `readyTimeoutMs` |
| Error `1235`, "mysql-ultralight does not yet support …" | The SQL uses something the engine does not implement yet. The message names it. [File an issue](https://github.com/CodeVachon/mysql-ultralight/issues) with the statement |
| A test sees rows it did not write | You are on the shared model. Move that file to per-file isolation, or assert on rows you created instead of on totals |
| Tests pass alone and fail together on one database | Concurrent connections are not isolated. See below |
| A socket path fails with a confusing OS error | Socket paths must stay under ~100 bytes. Use `socket: true`, which picks a short path under `/tmp` |

To see what your client actually sends, start with `trace: true` (or `--trace`). Every
command and result is logged to stderr.

## What it does not do

- **Transactions do not isolate concurrent connections.** A second connection sees
  uncommitted writes. A rollback restores the tables the transaction wrote. If another
  connection committed to one of those tables in the meantime, the rollback discards that
  work too. One database per test file avoids all of this.
- **Not yet supported:** explicit window frames (`ROWS`/`RANGE BETWEEN`), named `WINDOW`
  clauses, `NTH_VALUE`, a computed offset or default in `LAG`/`LEAD`, and `NESTED PATH`
  in `JSON_TABLE`. Each one fails with `1235`.
- **Computed columns have no declared width.** `SELECT UPPER(name) AS up` is described as
  `varchar(512)` where MySQL would say the column's real width. That only matters to
  tools that read column metadata.

[The working prototype](prototype.md) tracks the full list and is kept current.
