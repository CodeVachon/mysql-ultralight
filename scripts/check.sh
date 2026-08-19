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
    run_if_available "npm tests   " node bash -c 'node --test packages/npm/test/*.test.js'
fi

# The example is the only check that exercises the npm package, the wire
# protocol and Prisma Client at once. It needs its dependencies installed —
# reported as a skip rather than silently passing, and a failure under CI,
# which is the same rule every other tool here follows.
if [ -d examples/prisma-app ]; then
    if [ -d examples/prisma-app/node_modules ] && [ -x target/release/mysql-ultralight ]; then
        # MYSQL_ULTRALIGHT_BINARY pins the example to the binary this run built.
        # Without it, an installed platform package would win once the packages
        # are published, and the one check that covers every layer would be
        # testing the *released* engine rather than this commit.
        #
        # --no-install so a partial node_modules cannot make the gate fetch a
        # different Prisma from the registry.
        run "example     " env "MYSQL_ULTRALIGHT_BINARY=$PWD/target/release/mysql-ultralight" \
            bash -c 'cd examples/prisma-app && npx --no-install prisma generate >/dev/null && node --test test/*.test.js'
    elif [ "$REQUIRE_TOOLS" = 1 ]; then
        printf '  %s ... %s\n' "example     " "${RED}FAILED (no release binary, or examples/prisma-app not installed)${OFF}"
        FAILED+=("example")
    else
        printf '  %s ... %s\n' "example     " "${YELLOW}skipped (needs a release build and npm install in examples/prisma-app)${OFF}"
        SKIPPED+=("example")
    fi
fi

if [ "$FAST" = 0 ]; then
    # Unused dependencies. The closest analogue to what Fallow gives the JS
    # side; the Rust compiler will not tell you about these.
    run_if_available "unused deps " cargo-machete cargo machete
    # RustSec advisories against the lockfile.
    run_if_available "advisories  " cargo-audit cargo audit

    # Fallow analyses TypeScript and JavaScript only, so it runs against the
    # npm package and nothing else. Pointed at the Rust crates it would
    # discover no files and report a green score over nothing analysed, which
    # in a gate is worse than an error.
    if [ -d packages/npm ]; then
        run_if_available "npm fallow  " npx \
            bash -c 'cd packages/npm && npx --yes --quiet fallow@latest dead-code --fail-on-issues' 
    fi
fi

echo

if [ ${#SKIPPED[@]} -gt 0 ]; then
    printf '%s\n' "${YELLOW}Skipped: ${SKIPPED[*]}${OFF}"
    echo "  Install with: cargo install cargo-machete cargo-audit --locked"
    echo "  And, for the example: (cd examples/prisma-app && npm install)"
fi

if [ ${#FAILED[@]} -gt 0 ]; then
    printf '%s\n' "${RED}FAILED: ${FAILED[*]}${OFF}"
    exit 1
fi

printf '%s\n' "${GREEN}All checks passed.${OFF}"
