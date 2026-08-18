# Track A — Baseline: what real MySQL costs today

Measured 2026-08-18 against `mysql:8.3` on macOS 15.5 / Apple Silicon, using the Appello
config exactly: the same image, the same `provisions/mysql/my.cnf`, and the same
`--mysql-native-password=ON --sql-require-primary-key` flags. All measurements ran in
isolated containers on spare ports so the running dev stack was untouched.

This is the number mysql-ultralight has to beat. Without it, "faster" is a feeling.

## Results

| Scenario | Time |
|---|---|
| `docker run` returns | 0.10s |
| Empty container → first successful query | **4.5 – 5.4s** |
| Container with the 18MB dumps (200 tables) → first query | **8.9s** |
| Warm restart of an initialised container | **0.52s** |
| Container stop | 1.59s |
| `prisma migrate deploy`, 604 migrations | **26.1s** |
| Empty container → fully migrated schema | **30.6s** |
| Memory, idle after migration | **392 – 519 MB** |

Resulting schema: 277 tables, 581 foreign keys, 1,015 indexes, 1 view.

For comparison, mysql-ultralight today: **1.85ms** spawn to accepting connections, 636K
binary, from a 0.5MB process rather than half a gigabyte.

## Where the time actually goes

The container is not the expensive part. `apps/api/jest.setup.js` runs `globalSetup` once
per test run and does this:

1. `CREATE DATABASE TEST<epoch>` on the already-running MySQL — effectively free
2. `pnpm run prisma:deploy`, which is `prisma migrate deploy` followed by `prisma:updateViews`

Step 2 is **26 seconds of dead time before the first assertion executes**, paid on every
`pnpm test`. The container itself is usually already up, so its 4.5–8.9s is paid only on
machine restart and in CI.

Seeding is currently commented out in `jest.setup.js`, so that 26s buys an empty schema.

**The target this sets:** if a pre-baked state image boots in under 100ms, the setup path
goes from ~26s to ~0.1s — roughly **260× on the number developers actually wait for**. In
CI, where the container start is also paid, it is ~31s to ~0.1s.

Note the test suite uses one database for the whole run, not one per file. The
fresh-process-per-test-file model is a change from that, and a stricter isolation
guarantee than they have today.

## Things this turned up

### The provisioning dumps are broken

A fresh `docker compose up` on a clean volume **fails**. `3-anzen-views.sql` references
`u.defaultPaidLeavePayrollExport`, a column that does not exist in the 2024-11-14 structure
dump, and initdb aborts:

```
ERROR 1054 (42S22) at line 3157: Unknown column 'u.defaultPaidLeavePayrollExport'
in 'EXISTS subquery'
```

The dumps and the generated views file have drifted apart. Existing dev containers
predate this and are unaffected, so it is invisible until someone onboards or wipes a
volume. Not a mysql-ultralight issue, but worth someone's attention.

### sql_mode is not the default

`my.cnf` sets a mode that changes how SQL is *parsed*, not merely how it behaves:

```
REAL_AS_FLOAT, PIPES_AS_CONCAT, ANSI_QUOTES, IGNORE_SPACE, ONLY_FULL_GROUP_BY,
STRICT_ALL_TABLES, NO_ZERO_IN_DATE, NO_ZERO_DATE, ERROR_FOR_DIVISION_BY_ZERO,
NO_ENGINE_SUBSTITUTION
```

`ANSI_QUOTES` makes `"` an identifier quote. `PIPES_AS_CONCAT` makes `||` concatenation
rather than logical OR. Both produce a *valid parse with different meaning* if unimplemented
— the worst failure shape. Tracked as motte #0018.

### Correction: views are in scope, and the earlier feature scan was wrong

An earlier pass reported no views, based on line-oriented greps of the migrations
directory. That was wrong twice over. The migration that creates one writes

```sql
CREATE OR REPLACE
VIEW RecipientListFeed AS
```

with the keyword on the next line, which no single-line pattern matches. And views also
live in `provisions/`, which was never searched.

Rescanned with comments stripped and whitespace collapsed:

| Feature | Migrations (604 files) | provisions dumps |
|---|---|---|
| `CREATE VIEW` | 1 | 39 |
| `FOREIGN KEY` | 710 | 432 |
| `COLLATE` | 305 | 1,232 |
| multi-table `UPDATE ... JOIN` | 45 | 1 |
| `JSON_*` functions | 42 | – |
| `ON DUPLICATE KEY` | 14 | – |
| window functions `OVER()` | 7 | 1 |
| `ST_*` spatial functions | 4 | – |
| `GROUP_CONCAT` | 3 | 9 |
| generated columns | 3 | – |
| `CHECK` constraints | 3 | – |
| CTEs | 2 | 4 |
| `AUTO_INCREMENT` | 1 | – |

Absent from both: triggers, stored procedures, stored functions, events, fulltext
indexes, spatial indexes, and table partitioning. `WITH RECURSIVE` does not appear in
migrations or dumps — the one recursive CTE is in application code.

**Lesson for motte #0014:** the conformance checklist has to come from parsed SQL, not
greps, and it has to cover `provisions/` and the app's raw queries as well as
`migrations/`. Two of my three grep passes gave wrong answers.

## Reproducing

Scripts are in the session scratchpad rather than committed, since they hard-code local
paths and a dev password. The method: run `mysql:8.3` with the repo's `my.cnf` and flags
on a spare port, poll with a real client query as the app user until it succeeds, then
time `prisma migrate deploy` with `DATABASE_URL` pointed at that port.

Docker Desktop on this machine is allocated only 1.9 GiB, which a single migrated MySQL
fills to a quarter. Running more than two at once is tight.
