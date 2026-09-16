#!/usr/bin/env bash
#
# The single definition of "is this code clean".
#
# Called by the pre-commit hook, by CI, and by hand. Keeping one script rather
# than three copies of the command list is what stops the local gate and the CI
# gate from quietly drifting apart.
#
# Usage:
#   scripts/check.sh                 run every available check
#   scripts/check.sh --fast          skip the slower supply-chain checks
#   scripts/check.sh --fix           format and apply clippy's suggestions first
#   scripts/check.sh --require-tools treat a missing tool as a failure
#
# Exit code is 0 only if every check that ran passed. A missing tool is a skip
# locally and a failure under CI, because a gate that goes green over checks it
# never ran is worse than one that fails.

set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

# rustup installs here, and a git hook runs with a minimal PATH that will not
# include it.
export PATH="$HOME/.cargo/bin:$PATH"

# The binary a gate run is testing. Everything that spawns the engine -- the npm
# tests and every example -- is pinned to it, because that pin is the only thing
# stopping a published platform package from answering instead. One definition,
# so this script's callers cannot drift apart -- `scripts/differential.sh`,
# `scripts/ask.sh` and the npm package's own local-build fallback still look in
# `./target/release` and do not read CARGO_TARGET_DIR.
#
# CARGO_TARGET_DIR because a shared target directory is a common setup, and
# looking in the wrong place here does not fail: it reports every one of those
# checks as skipped, and the hint tells the reader to run the build they just
# ran. A `build.target-dir` in a cargo config is not read -- that would want
# `cargo metadata` and a JSON parser this script does not otherwise need.
#
# Made absolute: CARGO_TARGET_DIR is documented relative to the working
# directory, and the examples are run through a `cd`, so a relative pin passes
# the `-x` test at the repo root and then resolves to nothing inside the
# example -- a failure where the guard has just promised a skip.
CARGO_TARGET="${CARGO_TARGET_DIR:-target}"
case "$CARGO_TARGET" in
    /*) ;;
    *) CARGO_TARGET="$PWD/$CARGO_TARGET" ;;
esac
ENGINE="$CARGO_TARGET/release/mysql-ultralight"

FAST=0
FIX=0
# CI must not pass because a tool failed to install. Honour the conventional
# CI environment variable so the workflow needs no extra wiring.
REQUIRE_TOOLS=${CI:+1}
REQUIRE_TOOLS=${REQUIRE_TOOLS:-0}
for arg in "$@"; do
    case "$arg" in
        --fast) FAST=1 ;;
        --fix) FIX=1 ;;
        --require-tools) REQUIRE_TOOLS=1 ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

if [ -t 1 ]; then
    RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; DIM=$'\033[2m'; OFF=$'\033[0m'
else
    RED=''; GREEN=''; YELLOW=''; DIM=''; OFF=''
fi

FAILED=()
SKIPPED=()

# Runs one check, capturing output so a passing run stays quiet and a failing
# one shows everything.
run() {
    local name="$1"; shift
    printf '%s' "  ${name} ... "

    local output
    if output=$("$@" 2>&1); then
        printf '%s\n' "${GREEN}ok${OFF}"
        return 0
    fi

    printf '%s\n' "${RED}FAILED${OFF}"
    printf '%s\n' "${DIM}--- ${name} output ---${OFF}"
    printf '%s\n' "$output"
    printf '%s\n' "${DIM}--- end ${name} ---${OFF}"
    FAILED+=("$name")
    return 1
}

# Runs a check only if its tool is installed, recording a skip otherwise. The
# gate must not silently pass because a tool is missing, so skips are reported.
run_if_available() {
    local name="$1" tool="$2"; shift 2
    if ! command -v "$tool" >/dev/null 2>&1; then
        if [ "$REQUIRE_TOOLS" = 1 ]; then
            printf '  %s ... %s\n' "$name" "${RED}FAILED (${tool} not installed)${OFF}"
            FAILED+=("$name")
            return 1
        fi
        printf '  %s ... %s\n' "$name" "${YELLOW}skipped (${tool} not installed)${OFF}"
        SKIPPED+=("$name")
        return 0
    fi
    run "$name" "$@"
}

if [ "$FIX" = 1 ]; then
    echo "Applying fixes"
    cargo fmt --all
    cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged 2>/dev/null
    echo
fi

echo "Checking mysql-ultralight"

run "format      " cargo fmt --all --check
run "clippy      " cargo clippy --workspace --all-targets --all-features -- -D warnings
run "tests       " cargo test --workspace

# The npm package is real JavaScript, so it gets its own checks. Its tests
# spawn the release binary, so they need one built.
if [ -d packages/npm ]; then
    run_if_available "npm build   " cargo cargo build --release --quiet
    # Through a shell so the glob expands: this node treats a bare directory as
    # a module path rather than a directory of tests.
    #
    # The pin is the same one the examples get, and for the same reason: these
    # tests spawn a server through `resolveBinary`, so once the platform
    # packages are published an install under packages/npm would win and they
    # would be covering the released engine rather than this commit.
    #
    # With no binary at that path there is no unpinned fallback, because an
    # unpinned run is the state the pin exists to prevent: an `ok` from whatever
    # engine happened to be installed. Skipped instead, the way `run_example`
    # treats the same state, and a failure under --require-tools.
    #
    # The test is existence, not freshness. A binary left by an older build
    # satisfies it, so a run whose `npm build` was skipped for want of cargo
    # reports against that older build -- true of the examples since they were
    # written, and not something this pin set out to change.
    npm_tests_label="npm tests   "
    if [ -x "$ENGINE" ]; then
        run_if_available "$npm_tests_label" node \
            env "MYSQL_ULTRALIGHT_BINARY=$ENGINE" \
            bash -c 'node --test packages/npm/test/*.test.js'
    elif [ "$REQUIRE_TOOLS" = 1 ]; then
        printf '  %s ... %s\n' "$npm_tests_label" "${RED}FAILED (no release binary to pin the tests to)${OFF}"
        FAILED+=("$npm_tests_label")
    else
        printf '  %s ... %s\n' "$npm_tests_label" "${YELLOW}skipped (needs a release build)${OFF}"
        SKIPPED+=("$npm_tests_label")
    fi
fi

# The examples are the only checks that exercise the npm package, the wire
# protocol and a real client at once. Each needs its dependencies installed —
# reported as a skip rather than silently passing, and a failure under CI, which
# is the rule every other tool here follows.
#
# `node-test` answers "does the engine work"; `jest` and `vitest` answer "does
# the wiring still work", which breaks when the package's API moves.
run_example() {
    local label="$1" directory="$2" command="$3"

    if [ ! -d "$directory" ]; then
        return 0
    fi
    if [ -d "$directory/node_modules" ] && [ -x "$ENGINE" ]; then
        # MYSQL_ULTRALIGHT_BINARY pins the example to the binary this run built.
        # Without it an installed platform package would win once the packages
        # are published, and the checks that cover every layer would be testing
        # the *released* engine rather than this commit.
        run "$label" env "MYSQL_ULTRALIGHT_BINARY=$ENGINE" \
            bash -c "cd $directory && $command"
        return $?
    fi
    if [ "$REQUIRE_TOOLS" = 1 ]; then
        printf '  %s ... %s\n' "$label" "${RED}FAILED (no release binary, or $directory not installed)${OFF}"
        FAILED+=("$label")
        return 1
    fi
    printf '  %s ... %s\n' "$label" "${YELLOW}skipped (needs a release build and npm install in $directory)${OFF}"
    SKIPPED+=("$label")
}

# The app is built and baked once; every runner's `pretest` then re-bakes,
# which is a no-op after the first. Typechecked here because Vitest transpiles
# without checking, so a type error would otherwise ship green.
run_example "app         " examples/app 'npm run build --silent && npm run typecheck --silent'
run_example "node:test   " examples/node-test 'npm test --silent'
run_example "jest        " examples/jest 'npm run typecheck --silent && npm test --silent'
run_example "vitest      " examples/vitest 'npm run typecheck --silent && npm test --silent' 

# The docs site is built from `docs/` by CI, so the gate builds it too: a
# SUMMARY.md entry naming a file that does not exist should fail here rather
# than after the push. Built into a temporary directory because the gate is a
# check, not a way to produce the site.
if [ -f book.toml ]; then
    run_if_available "docs        " mdbook \
        bash -c 'out=$(mktemp -d); trap "rm -rf \"$out\"" EXIT; mdbook build --dest-dir "$out"'
fi

if [ "$FAST" = 0 ]; then
    # Unused dependencies. The closest analogue to what Fallow gives the JS
    # side; the Rust compiler will not tell you about these.
    run_if_available "unused deps " cargo-machete cargo machete
    # RustSec advisories against the lockfile.
    run_if_available "advisories  " cargo-audit cargo audit

    # Fallow analyses TypeScript and JavaScript only, so it runs over the npm
    # package and the examples and nothing else. Pointed at the Rust crates it
    # would discover no files and report a green score over nothing analysed,
    # which in a gate is worse than an error.
    for javascript in packages/npm examples/app examples/jest examples/vitest; do
        if [ -d "$javascript" ]; then
            printf -v label 'fallow %-6s' "$(basename "$javascript")"
            run_if_available "$label" npx \
                bash -c "cd $javascript && npx --yes --quiet fallow@latest dead-code --fail-on-issues"
        fi
    done
fi

echo

# The install hints are single-quoted on purpose. They are shell for the reader
# to run, not for this script to expand: a `$d` in double quotes is an unbound
# variable under `set -u`, which aborted here with no verdict printed -- so the
# one branch written to degrade gracefully was the only one that crashed, and it
# crashed exactly where a fresh clone missing a tool would land. See #0164.
if [ ${#SKIPPED[@]} -gt 0 ]; then
    printf '%s\n' "${YELLOW}Skipped: ${SKIPPED[*]}${OFF}"
    echo "  Install with: cargo install cargo-machete cargo-audit mdbook --locked"
    echo '  And, for the examples: for d in examples/*/; do npm install --prefix "$d"; done'
    echo '  A skip that names a release build wants: cargo build --release'
fi

if [ ${#FAILED[@]} -gt 0 ]; then
    printf '%s\n' "${RED}FAILED: ${FAILED[*]}${OFF}"
    exit 1
fi

printf '%s\n' "${GREEN}All checks passed.${OFF}"
