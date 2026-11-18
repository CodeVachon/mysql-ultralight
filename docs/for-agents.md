# For AI agents

This page is for a coding agent that has been asked to put mysql-ultralight under a test
suite. It gives the facts, the decisions, the recipes and the failure modes, and leaves
out the narrative. [Using it in a test suite](guide.md) is the same material written for
a person. The raw markdown of this page is at
<https://raw.githubusercontent.com/CodeVachon/mysql-ultralight/main/docs/for-agents.md>.

## Facts

- An in-memory server that speaks the MySQL 8 wire protocol and accepts MySQL 8 SQL.
  Only for tests: it keeps nothing on disk and shares one catalog across connections.
- npm package: `@codevachon/mysql-ultralight`. It exports `start`, `Server`,
  `resolveBinary`, `platformKey` and `PLATFORMS`, and ships types in `index.d.ts`.
  CommonJS, and ESM `import` also works. Node 18 or newer.
- Supported platforms: `darwin-arm64`, `linux-x64-gnu`, `linux-x64-musl`,
  `linux-arm64-gnu`. Windows and `darwin-x64` are not supported.
- Startup is milliseconds, including booting from a baked image of a schema with hundreds
  of tables. Measurements: [prototype.md](prototype.md).
- With no `password`, the server accepts any credentials. `Server.url` uses user `root`.
- SQL the engine does not support fails with MySQL error **1235** (SQLSTATE `0A000`) and
  a message naming the feature. It does not return wrong answers.

## Decisions

Apply these rules in order:

1. **Does the project use MySQL 8 in its tests?** If not, stop. This replaces MySQL and
   nothing else.
2. **Is the platform supported?** If it is not, and there is no Rust toolchain to build
   from source, stop and tell the user.
3. **Is there a migration history (Prisma, Knex, raw SQL)?** If so, bake a state image
   before the runner starts. Never migrate inside a test hook.
4. **Shared or per-file model?**
   - Start with the **shared** model when an existing suite already has a `globalSetup`
     that brings up MySQL. Change only the inside of that function.
   - Use **per-file** when the user wants isolation, when tests assert exact counts, or
     when tests collide on unique values across files. This needs the application code
     to accept its DB client as a parameter. If it imports a module-level client, tell
     the user that this has to change first, and don't work around it.
5. **Port:** always leave it at the default `0`. Never pin a port in test code.

## Recipes

### Install

```sh
npm install --save-dev @codevachon/mysql-ultralight
```

If npm returns 404 (no release published yet), install from a checkout:

```sh
git clone https://github.com/CodeVachon/mysql-ultralight /path/to/mysql-ultralight
cargo build --release --manifest-path /path/to/mysql-ultralight/Cargo.toml
npm install --save-dev /path/to/mysql-ultralight/packages/npm
```

The local install is a symlink, so the package finds `target/release/mysql-ultralight`
without further setup. In any other layout, set
`MYSQL_ULTRALIGHT_BINARY=/abs/path/to/mysql-ultralight`.

### Start, connect, stop

```js
const { start } = require('@codevachon/mysql-ultralight');

const db = await start({ database: 'app' });   // resolves when it accepts connections
process.env.DATABASE_URL = db.url;             // Prisma: use as-is
await db.stop();                               // idempotent
```

For `mysql2` and other drivers that don't read a `?socket=` URL parameter, pass the
fields: `{ host: '127.0.0.1', port: db.port, user: 'root', database: db.database }`, or
`{ socketPath: db.socketPath, … }` when `socket` was set.

### Bake a state image (once, before the runner)

```js
// scripts/bake.js: complete. Assumes Prisma and prisma/migrations/.
const { start, resolveBinary } = require('@codevachon/mysql-ultralight');
const { PrismaClient } = require('@prisma/client');
const { execFileSync } = require('node:child_process');
const { createHash } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

// A hash of the migrations and the engine binary. Tests import this too.
function imagePath() {
    const dir = path.resolve('prisma/migrations');
    const hash = createHash('sha256');
    const walk = (d) => {
        const entries = fs.readdirSync(d, { withFileTypes: true });
        entries.sort((a, b) => a.name.localeCompare(b.name));
        for (const e of entries) {
            const full = path.join(d, e.name);
            if (e.isDirectory()) walk(full);
            else hash.update(path.relative(dir, full)).update(fs.readFileSync(full));
        }
    };
    walk(dir);
    const bin = fs.statSync(resolveBinary());
    hash.update(`${bin.size}:${bin.mtimeMs}`);
    return path.resolve('.cache', `${hash.digest('hex').slice(0, 16)}.mus`); // ABSOLUTE
}

async function bake() {
    const image = imagePath();
    if (fs.existsSync(image)) return;
    fs.mkdirSync(path.dirname(image), { recursive: true }); // the server does not create it
    const s = await start({ database: 'app' });
    try {
        execFileSync('npx', ['--no-install', 'prisma', 'migrate', 'deploy'], {
            env: { ...process.env, DATABASE_URL: s.url }, stdio: 'inherit'
        });
        const prisma = new PrismaClient({ datasources: { db: { url: s.url } } });
        try {
            // A SQL string literal: double any single quote in the path.
            await prisma.$queryRawUnsafe(`SELECT ultralight_snapshot('${image.replace(/'/g, "''")}')`);
        } finally {
            await prisma.$disconnect();
        }
    } finally {
        await s.stop();
    }
}

module.exports = { imagePath };
if (require.main === module) {
    bake().catch((e) => { console.error(e); process.exit(1); });
}
```

Wire it as `"pretest": "node scripts/bake.js"` in `package.json`. Tests get the path from
`require('./scripts/bake.js').imagePath()`. Reference implementation:
[`examples/app/src/testing.ts`](https://github.com/CodeVachon/mysql-ultralight/blob/main/examples/app/src/testing.ts)
(`fingerprint`, `bake`, `startDatabase`).

### Boot from the image (in tests)

```js
if (!fs.existsSync(image)) throw new Error(`no image at ${image}; run the bake first`);
const db = await start({ image, database: 'app' });
```

Pass the `database` name the image was baked with. A different name does **not** fail:
the server loads the image and also creates an empty database under the new name, and
`db.url` points at the empty one. The symptom is "table does not exist" on the first
query.

### Shared model

| Runner | Wiring |
|---|---|
| Jest | `globalSetup`: `start`, set `process.env.DATABASE_URL`, store the server on `globalThis`. `globalTeardown`: `globalThis.__DB__?.stop()` |
| Vitest | `globalSetup` file: `start`, set `process.env.DATABASE_URL`, `return () => db.stop()` |

Set the environment variable before workers fork. Both runners' `globalSetup` run early
enough for that.

### Per-file model

Register this in Jest `setupFilesAfterEnv` or Vitest `setupFiles`. For `node:test`, put
it in each file with `before`/`after`.

```ts
let server, starting;
beforeAll(async () => { starting = start({ image, database: 'app' }); server = await starting; });
afterAll(async () => {
    const s = server ?? (await starting?.catch(() => undefined));
    await s?.stop();
});
```

Keep the `starting` promise. A timed-out `beforeAll` still produces a server, and without
the promise nothing would stop it.

Working, tested versions:
[`examples/jest`](https://github.com/CodeVachon/mysql-ultralight/tree/main/examples/jest),
[`examples/vitest`](https://github.com/CodeVachon/mysql-ultralight/tree/main/examples/vitest),
[`examples/node-test`](https://github.com/CodeVachon/mysql-ultralight/tree/main/examples/node-test).

## Never do these

- **Don't pin a port in tests.** Parallel workers will collide on it.
- **Don't pass a relative path to `ultralight_snapshot` or `ultralight_restore`.** The
  server resolves it against its own working directory.
- **Don't migrate inside `beforeAll` or `globalSetup`.** It runs on every cold worker,
  takes seconds, and hits hook timeouts that look like flakes.
- **Don't fall back to baking when the image is missing.** Fail with a message that names
  the bake command.
- **Don't reuse an image across engine upgrades or migration changes.** Put both in the
  file name's hash. Only a format change is refused at load, so an old image from an
  upgraded engine loads silently.
- **Don't skip `stop()`.** A server that is never stopped keeps the Node process running.
- **Don't call `ultralight_restore` on a server that other test files are using at the
  same time.** It resets their data too.
- **Don't rely on transaction isolation between concurrent connections.** It does not
  exist here. If a test needs it, that test needs a real MySQL.
- **Don't rewrite the user's SQL to avoid a 1235 error without telling them.** Report the
  statement and the message. That is a gap in the engine, not a bug in their code.
- **Don't run this in production, or point it at data that matters.**

## Errors and what to do

| Message contains | Do this |
|---|---|
| `no state image at` | Run the bake. Check that `pretest` is wired |
| `the binary for … is missing` | Reinstall without `--omit=optional`, or set `MYSQL_ULTRALIGHT_BINARY` |
| `no prebuilt binary for` | Unsupported platform. Build from source and set `MYSQL_ULTRALIGHT_BINARY` |
| `MYSQL_ULTRALIGHT_BINARY points at` | Fix the environment variable's path |
| `exited … before it was ready` | Read the stderr included in the error. Usually a pinned port in use, or an image in an older format, which means rebake |
| `did not start within` | Raise `readyTimeoutMs` |
| `could not read the port` | The binary is not mysql-ultralight. Check `MYSQL_ULTRALIGHT_BINARY` |
| MySQL error `1235`, `mysql-ultralight does not yet support …` | Unsupported SQL. Report the statement to the user, and suggest filing it at <https://github.com/CodeVachon/mysql-ultralight/issues> |

To see the traffic, start with `trace: true` (CLI: `--trace`). Every command and result
goes to stderr.

## Reference

`start(options)` options: `image`, `database` (`'test'`), `port` (`0`), `socket`
(`false` \| `true` \| path under ~100 bytes), `password`, `serverVersion`, `trace`
(`false`), `binary`, `readyTimeoutMs` (`10000`).

`Server`: `url`, `port`, `socketPath`, `database`, `password`, `pid`, `startupMs`,
`exited`, `child`, `stop({ timeoutMs = 5000 })`.

SQL: `SELECT ultralight_snapshot('/abs.mus')`, `SELECT ultralight_restore('/abs.mus')`,
`SELECT @@ultralight_instance`.

CLI: `mysql-ultralight [--port N] [--socket P] [--database D] [--state P] [--password X]
[--server-version V] [--trace]`. When ready it prints one stdout line:
`ready port=N socket=P|- startup_ms=F instance=ID`.

Not supported yet (each fails with 1235): `ROWS`/`RANGE BETWEEN` window frames, named
`WINDOW` clauses, `NTH_VALUE`, a computed offset or default in `LAG`/`LEAD`, and
`JSON_TABLE … NESTED PATH`.
