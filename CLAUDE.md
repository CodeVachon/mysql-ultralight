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

Flags: `--fast` skips the supply-chain checks, `--fix` formats and applies clippy
suggestions first, `--require-tools` turns a missing tool into a failure rather than a
skip. CI sets that automatically via the `CI` environment variable, so a tool that fails
to install fails the build instead of quietly reducing coverage.

### Fallow does not apply here

The user's other projects gate on [Fallow](https://docs.fallow.tools). **Fallow is
TypeScript/JavaScript only.** Run against this Rust repo it discovers zero files and
reports a green "Health score: 100 A" over nothing analyzed — a false pass, which is
worse than an error in a gate. `cargo machete` and `cargo audit` replace it.

If a JS/TS surface ever lands here — the npm distribution wrapper in Phase 7 will be real
JavaScript — Fallow becomes appropriate **for that subtree** and should be added to
`scripts/check.sh` then.

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
| `crates/cli` | The `mysql-ultralight` binary |

## Conventions

- **Comments explain why, not what.** The protocol is full of decisions that look
  arbitrary without the reason — the two-bit offset in binary result bitmaps, why 251
  cannot use the one-byte length encoding. Those reasons belong in the code.
- **Protocol correctness gets a test.** A desynchronised connection fails far from its
  cause, so framing, length encoding, and bitmap offsets are tested directly rather than
  only through end-to-end paths.
- **Unsupported means a clear error**, never a wrong answer. `ErrPacket::unsupported`
  exists so gaps surface as actionable messages.
- The conformance target is the real Appello schema at
  `~/Repositories/Appello/application-mono-repo/apps/api/prisma` — 203 models, 605
  migrations. See `docs/spike-wire-protocol.md`.
