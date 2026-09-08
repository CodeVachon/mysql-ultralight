# mysql-ultralight

A fast-starting, MySQL 8 compatible storage engine for tests. Rust, in-memory, no
durability. Not for production, ever.

Work is tracked in [motte](https://codevachon.github.io/motte), not in this file.
Run `motte ready` to see what is actionable and `motte show <ref>` for detail.

## The commit gate

**Every commit must pass the full gate. No exceptions without the user saying so.**

For each commit:

1. **Run `/code-review`** on the change.
2. **Apply every finding it reports**, then state in the commit report what was changed
   as a result. This is the user's explicit standing instruction — do not commit with
   known findings outstanding.
3. **Run `./scripts/check.sh`** and get a clean pass.
4. Commit.

`scripts/check.sh` is the single definition of clean. The pre-commit hook and CI both
call it, so it must never diverge from what they need:

| Check | Tool |
|---|---|
| Formatting | `cargo fmt --all --check` |
| Lints, dead code, complexity | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| Tests | `cargo test --workspace` |
| Unused dependencies | `cargo machete` |
| Security advisories | `cargo audit` |
| npm package tests | `node --test packages/npm/test/*.test.js` |
| npm dead code | `fallow dead-code` in `packages/npm` |
| Prisma end to end | `npm test` in `examples/node-test` |
| Runner wiring | `npm test` in `examples/jest` and `examples/vitest` |

`scripts/differential.sh` is deliberately *not* in the gate: it runs the SQL
corpus against a real `mysql:8.3` in Docker and compares the answers, which a
checkout cannot be assumed to be able to do. Run it when changing anything the
engine answers with — it is the only thing here that checks this engine against
MySQL rather than against what somebody remembered about MySQL. See
[`crates/differential`](crates/differential/README.md).

Flags: `--fast` skips the supply-chain checks, `--fix` formats and applies clippy
suggestions first, `--require-tools` turns a missing tool into a failure rather than a
skip. CI sets that automatically via the `CI` environment variable, so a tool that fails
to install fails the build instead of quietly reducing coverage.

### Measuring a premise before working it

`scripts/ask.sh` asks MySQL 8.3 and this engine the same SQL, with the same
client, and prints the two answers side by side.

```sh
scripts/ask.sh probe.sql             # compare rows
COLINFO=1 scripts/ask.sh probe.sql   # compare column metadata too
```

It settles one question now, where `scripts/differential.sh` keeps every
settled question settled. **Run it against an issue's claims before
implementing them.** An issue body is prose somebody wrote once, and in this
project that prose has been wrong often enough to be the main cost of a task:
issues have turned out already fixed, half fixed, wrong about which side
diverged, or right about the divergence and wrong about the value. A
validation pass over Epic 9 found one ticket whose central example — a bit
literal said to evaluate to `5` — describes something MySQL does not do, and
implementing it as written would have produced a new wrong answer.

Three ways a probe lies, each of which produced a false pass before the script
guarded against it:

- A probe made only of `INSERT`/`UPDATE`/DDL **compares nothing**, because the
  client prints nothing for a statement with no result set. Ask `ROW_COUNT()`
  or select the table back. Two empty outputs are reported `INCONCLUSIVE`
  rather than as agreement.
- A claim about a column's **type, collation, or flags is invisible in the
  rows** — the values agree and the metadata still differs. Use `COLINFO=1`.
- The probe must **exercise the claim**: a test of `INSERT IGNORE` skipping a
  row is worthless if the conflicting row was deleted first, and it will
  cheerfully report agreement.

The client charset is pinned to `utf8mb4` because the container's client
defaults to latin1 and reports `latin1_swedish_ci` for every string column.
That is not a divergence, it is the client — and it is how #0074 came to be
recorded against values a latin1 client had produced.

### Proving a test bites

`scripts/mutate.sh` removes a piece of the implementation, runs a test, and puts it
back. A passing test is only evidence if it would have failed without the code it
covers, and several here did not: assertions that never ran, a test reading a struct
field where the bug was in the wire flag, and one whose sample values sorted the same
way whether the comparison was right or wrong.

```sh
scripts/mutate.sh <file> <expected-lines> <find> <replace> -- <command...>
```

It refuses unless the number of matching lines is the number given, which is the part
that matters. The way this check fails is *under*-mutating — a replacement that hits
one of two call sites leaves the mechanism working, so the test passes and reads
exactly like a test that does not bite. Exits 0 when the command fails, because that
is the outcome being looked for.

### Fallow applies to `packages/npm` only

The user's other projects gate on [Fallow](https://docs.fallow.tools). **Fallow is
TypeScript/JavaScript only.** Run against the Rust crates it discovers zero files and
reports a green "Health score: 100 A" over nothing analyzed — a false pass, which is
worse than an error in a gate. `cargo machete` and `cargo audit` cover the Rust side.

The npm package and the examples are real JavaScript, so `scripts/check.sh` runs Fallow
against `packages/npm` and each example that has source of its own. Its config lives in `packages/npm/.fallowrc.jsonc`; the
platform packages are listed in `ignoreDependencies` because they are resolved at runtime
by a name built from a template string, which no static analysis can see.

## Hooks

The pre-commit hook is version controlled in `.githooks/`, not `.git/hooks/`. A fresh
clone needs:

```sh
git config core.hooksPath .githooks
```

## Layout

| Crate | Responsibility |
|---|---|
| `crates/protocol` | Wire codec: framing, capabilities, auth, handshake, columns, values |
| `crates/server` | Connection state machine, command dispatch, tracing |
| `crates/engine` | Catalog, executor, SQL rewriting, and the state image |
| `crates/cli` | The `mysql-ultralight` binary |
| `crates/conformance` | Extracts the SQL surface of a target codebase |
| `crates/differential` | Runs the same SQL against MySQL and this engine and diffs the answers |
| `packages/npm` | `@codevachon/mysql-ultralight`: the JS API and the platform packages |
| `examples/app` | The example application: schema, migrations, controllers, and how a test gets a database |
| `examples/node-test` | Tests only. The engine-level claims: image equals migrations, no drift |
| `examples/jest` | Tests only, shared and per-file |
| `examples/vitest` | Tests only, the same two models |

## Conventions

- **Comments explain why, not what.** The protocol is full of decisions that look
  arbitrary without the reason — the two-bit offset in binary result bitmaps, why 251
  cannot use the one-byte length encoding. Those reasons belong in the code.
- **Protocol correctness gets a test.** A desynchronised connection fails far from its
  cause, so framing, length encoding, and bitmap offsets are tested directly rather than
  only through end-to-end paths.
- **Unsupported means a clear error**, never a wrong answer. `ErrPacket::unsupported`
  exists so gaps surface as actionable messages.
- **One app, many runners.** `examples/app` owns the schema, the migrations and the
  controllers; every runner example contains nothing but tests. A schema copied per
  runner is a schema that drifts per runner, and comparing runners is only meaningful
  when the thing under test is identical.
- **The state image format is append-only.** Field order in `crates/engine/src/image.rs`
  *is* the format. Nothing may be reordered and nothing inserted in the middle; a change
  means a new `FORMAT_VERSION`. Readers bind fields to `let` bindings in write order for
  exactly this reason — a struct literal would make a reordering silent.
- **Bounds on one resource go into one budget.** Two limits guarding the same stack
  compose, and the composition is what a client sends. Measured twice, both reverted: a
  depth limit plus a separate flat-chain length limit let a chain-of-parenthesised-chains
  pay one level of depth per 256 terms and abort the process at ~1500; a set-operation
  spine checked against its own copy of the limit did the same. Anything that costs frames
  is added to the same counter, or it composes with everything that is. The corollary for
  reviews: when a change adds a second limit, the question is not whether each is right
  but what passes both.
- **The engine's limits assume the stack the binary gives a connection**, 32 MB, because
  the npm platform packages spawn the CLI and that is the only way it ships. A test that
  walks a deep statement must spawn a thread that size; on the 2 MB a `cargo test` thread
  gets, the walkers run out at about ninety terms and abort the *test binary* rather than
  failing an assertion. #0253 tracks what this costs an embedder.
- The conformance target is the real Appello schema at
  `~/Repositories/Appello/application-mono-repo/apps/api/prisma` — 203 models, 604
  migrations (`ls` counts 605; one entry is `migration_lock.toml`). See `docs/spike-wire-protocol.md`.
