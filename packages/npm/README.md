# @codevachon/mysql-ultralight

A fast-starting, MySQL 8 compatible in-memory database for tests.

MySQL is slow and heavy to spin up for a test suite. This starts in about a
millisecond, speaks the real MySQL wire protocol, and holds everything in
memory. It is for tests. It has no durability and is not for production, ever.

```sh
npm install --save-dev @codevachon/mysql-ultralight
```

## Use it

```js
import { start } from '@codevachon/mysql-ultralight';

const db = await start();
process.env.DATABASE_URL = db.url; // mysql://root@127.0.0.1:53412/test

// ... run your tests ...

await db.stop();
```

The default port is 0, which asks the OS for a free one. Parallel test workers
therefore cannot race for a port, and `start()` resolves only once the server is
accepting connections — there is nothing to poll and no sleep to tune.

## Skip the migrations

This is the reason the project exists. Replaying a migration history costs
seconds per test process; loading its result costs about a millisecond.

Bake once, from a real `prisma migrate deploy` so the image is exactly what your
migrations produce:

```js
import { start } from '@codevachon/mysql-ultralight';
import { execFileSync } from 'node:child_process';

const db = await start({ port: 3306, database: 'app' });
execFileSync('npx', ['prisma', 'migrate', 'deploy'], {
    env: { ...process.env, DATABASE_URL: db.url },
    stdio: 'inherit'
});
// Any MySQL client can take the snapshot; it is an ordinary SELECT.
await query(db, "SELECT ultralight_snapshot('.cache/schema.mus')");
await db.stop();
```

Then boot from it in every test process:

```js
const db = await start({ image: '.cache/schema.mus', database: 'app' });
```

Against a real 604-migration Prisma history that is **26.1s of `migrate deploy`
down to about 1ms**, with 278 tables, 2,771 columns, seed rows and every
constraint intact.

Key the image file by a hash of your migrations directory, and rebake when it
changes — a stale image is a schema your tests are not actually running against.

`SELECT ultralight_restore('.cache/schema.mus')` does the same to a *running*
server, which resets it between test files without starting a new process.

## With jest

```js
// jest.globalSetup.js
import { start } from '@codevachon/mysql-ultralight';

export default async function setup() {
    const db = await start({ image: '.cache/schema.mus', database: 'app' });
    process.env.DATABASE_URL = db.url;
    globalThis.__DB__ = db;
}

// jest.globalTeardown.js
export default async function teardown() {
    await globalThis.__DB__?.stop();
}
```

For per-file isolation, start one server per file — that is what it is built
for — or use a unix socket to avoid ports entirely:

```js
const db = await start({ socket: true });
```

## API

### `start(options?): Promise<Server>`

| Option | Default | |
|---|---|---|
| `image` | — | Boot from a baked state image |
| `database` | `test` | Database created at startup |
| `port` | `0` | TCP port; 0 picks a free one |
| `socket` | `false` | `true` for a temporary path, or a path of your own |
| `password` | — | Require this password; omitted, any credentials work |
| `serverVersion` | — | Version string reported to clients |
| `trace` | `false` | Log every command to stderr |
| `binary` | — | Use this binary instead of the resolved one |
| `readyTimeoutMs` | `10000` | How long to wait for startup |

### `Server`

- `url` — a connection URL, over the socket where there is one
- `port`, `socketPath`, `pid`, `database`
- `startupMs` — what the server reported for its own startup
- `stop(options?)` — stops it and waits; safe to call twice

## The CLI

The same binary, on your PATH:

```sh
npx mysql-ultralight --state .cache/schema.mus --socket /tmp/app.sock
npx mysql-ultralight --help
```

It prints one line when it is up, so a shell script can wait on it without
polling:

```
ready port=53412 socket=- startup_ms=0.932 instance=3f1c0a9b6d2e4517
```

## Platforms

Prebuilt binaries ship as optional dependencies for macOS arm64 and Linux
x64/arm64, glibc and musl. Windows and macOS x64 are not supported. On anything
else, build with `cargo build --release` and point `MYSQL_ULTRALIGHT_BINARY` at
the result.

## What it supports

Enough of MySQL 8 to run a real Prisma application's migrations and queries:
joins, `GROUP BY` with functional dependency, window functions, CTEs, set
operators, subqueries, views, foreign keys with cascades, transactions with
savepoints, generated columns, `CHECK` constraints, multi-table DML, prepared
statements, `information_schema`, and `sql_mode` handling.

What it does not support yet, it refuses with a clear error rather than
returning a wrong answer — the one failure mode a test database must not have.

## Licence

MIT OR Apache-2.0
