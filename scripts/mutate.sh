#!/usr/bin/env bash
# Break something on purpose, run a test, and put it back.
#
# A test that passes is only evidence if it would have failed without the code
# it is testing. The way to know is to remove that code and watch it fail —
# and the way *that* goes wrong is removing less than you meant to: a
# replacement that matches one of two call sites leaves the mechanism working,
# the test passes, and it reads exactly like a test that does not bite.
#
# So this refuses to run unless the number of lines it changed is the number
# you said it would be.
#
#   scripts/mutate.sh crates/engine/src/value.rs 2 \
#       'Value::Float(_) | Value::Decimal(_)' 'Value::Float(_)' \
#       -- cargo test -p mysql-ultralight-engine --test sql a_decimal
#
# Exits 0 when the command *failed*, which is the outcome that means the test
# bites. The file is restored however this ends.
set -uo pipefail

if [ "$#" -lt 5 ]; then
    echo "usage: $0 <file> <expected-lines> <find> <replace> -- <command...>" >&2
    exit 2
fi

file=$1 expected=$2 find=$3 replace=$4
shift 4
[ "${1:-}" = "--" ] && shift

if [ ! -f "$file" ]; then
    echo "$file does not exist" >&2
    exit 2
fi

# Checked, because everything after this mutates a file that only the backup
# can put back.
if ! backup=$(mktemp) || [ -z "$backup" ] || ! cp "$file" "$backup"; then
    echo "could not take a backup of $file" >&2
    exit 2
fi
# Restored on any exit, including an interrupt: a mutated source left behind is
# worse than no check at all, because the next run measures the wrong thing.
trap 'cp "$backup" "$file"; rm -f "$backup"' EXIT INT TERM

# Before the comparison, because `[ x -ne 1 ]` exits 2 on a non-number and an
# `if` reads that as false — so a mistyped or swapped argument would skip the
# one check this script exists to make.
case "$expected" in
    '' | *[!0-9]*)
        echo "expected-lines must be a number, got: $expected" >&2
        exit 2
        ;;
esac

matched=$(grep -F -c -- "$find" "$file" || true)
if [ "$matched" -ne "$expected" ]; then
    echo "expected to change $expected line(s), found $matched matching:" >&2
    echo "    $find" >&2
    exit 2
fi

if ! python3 - "$file" "$find" "$replace" <<'PY'
import sys
path, find, replace = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path) as f:
    text = f.read()
with open(path, "w") as f:
    f.write(text.replace(find, replace))
PY
then
    # Unmutated, so the test would pass and this would report that as a test
    # that covers nothing — the exact wrong answer.
    echo "could not apply the mutation" >&2
    exit 2
fi

echo "mutated $matched line(s) in $file, running: $*"
if "$@"; then
    echo
    echo "PASSED WITH THE MUTATION IN PLACE — the test does not cover this." >&2
    exit 1
fi
echo
echo "failed with the mutation in place, as it should"
