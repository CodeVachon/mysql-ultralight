#!/usr/bin/env bash
#
# Runs the differential harness: identical SQL against a real MySQL and against
# mysql-ultralight, reporting where they disagree.
#
# Not part of scripts/check.sh, because it needs a MySQL to compare against and
# a checkout does not have one. This starts one in Docker and stops it again.
#
#   ./scripts/differential.sh              # start MySQL, run, tear down
#   ./scripts/differential.sh --keep       # leave MySQL running for the next run
#
# --keep applies to MySQL and to nothing else. MySQL takes seconds to come up,
# which is worth reusing; the engine takes milliseconds, so reusing one would
# save nothing and cost the only guarantee this harness makes -- a run that
# reached a server left behind by an earlier one compares MySQL against a build
# nobody is running and reports it as agreement.
#
set -euo pipefail
cd "$(dirname "$0")/.."

REF_PORT=${REF_PORT:-33306}
ENGINE_PORT=${ENGINE_PORT:-33307}
IMAGE=${MYSQL_IMAGE:-mysql:8.3}
CONTAINER=mysql-ultralight-differential
KEEP=0
EXTRA=()
for arg in "$@"; do
    case "$arg" in
        --keep) KEEP=1 ;;
        *) EXTRA+=("$arg") ;;
    esac
done

if ! docker info >/dev/null 2>&1; then
    echo "docker is not running; this needs it to start ${IMAGE}" >&2
    exit 2
fi

started_here=0
engine_pid=
engine_log=

# Installed before anything that could leave a container or a process behind:
# under `set -e` a failed build between `docker run` and a later trap would
# exit with the container still up.
cleanup() {
    if [ -n "$engine_pid" ]; then
        # `wait` after the kill so bash reaps the job quietly; without it the
        # shell prints a "Terminated" line over the harness's own report.
        kill "$engine_pid" 2>/dev/null || true
        wait "$engine_pid" 2>/dev/null || true
    fi
    [ -n "$engine_log" ] && rm -f "$engine_log"
    if [ "$started_here" = 1 ] && [ "$KEEP" = 0 ]; then
        docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

if ! docker exec "$CONTAINER" mysqladmin ping -h 127.0.0.1 --silent >/dev/null 2>&1; then
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
    echo "starting ${IMAGE} on port ${REF_PORT}"
    docker run -d --name "$CONTAINER" \
        -e MYSQL_ALLOW_EMPTY_PASSWORD=1 \
        -p "${REF_PORT}:3306" "$IMAGE" >/dev/null
    started_here=1
    for _ in $(seq 1 120); do
        docker exec "$CONTAINER" mysqladmin ping -h 127.0.0.1 --silent >/dev/null 2>&1 && break
        sleep 1
    done
fi

cargo build --release -q -p mysql-ultralight-cli
# Kept rather than discarded: the ready line is read back out of it, and a port
# already in use makes the engine exit before printing one -- without its output
# that failure would be a timeout with no reason attached to it.
engine_log=$(mktemp)
./target/release/mysql-ultralight --port "$ENGINE_PORT" --database differential >"$engine_log" 2>&1 &
engine_pid=$!

# Waits for *this* process to say it is ready, rather than for something to
# accept a connection on the port. Those are different questions, and asking
# the second one is how a run once compared MySQL against an engine left over
# from an earlier debugging session: the new engine failed to bind, exited, and
# the stale one answered the poll. Reading the ready line instead means the
# port cannot have been taken, since this process could not have bound it and
# printed that line otherwise -- and the line carries the instance id, which is
# how the harness proves the connection it makes lands here too.
instance=
for _ in $(seq 1 100); do
    line=$(grep -m1 '^ready ' "$engine_log" 2>/dev/null || true)
    if [ -n "$line" ]; then
        # Matched rather than stripped: `${line##*instance=}` on a line without
        # the field hands back the whole line, and the harness would then be
        # told the engine's id is the word "ready".
        case "$line" in
            *" instance="*)
                instance=${line##* instance=}
                instance=${instance%% *}
                ;;
            *)
                echo "the engine's ready line carries no instance id: $line" >&2
                exit 2
                ;;
        esac
        break
    fi
    # Checked after the log, not before: a server that starts and reports in
    # under one poll interval would otherwise be declared dead.
    kill -0 "$engine_pid" 2>/dev/null || break
    sleep 0.1
done
if [ -z "$instance" ]; then
    echo "the engine did not come up on port ${ENGINE_PORT}; it said:" >&2
    cat "$engine_log" >&2
    echo "" >&2
    echo "if that is an address already in use, a mysql-ultralight from an earlier" >&2
    echo "run still holds the port. Stop it rather than comparing against it." >&2
    exit 2
fi

# `${EXTRA[@]}` on an empty array is an unbound variable under `set -u` in the
# bash macOS ships, so the expansion is guarded rather than the option dropped.
cargo run --release -q -p mysql-ultralight-differential -- \
    --reference "mysql://root@127.0.0.1:${REF_PORT}" \
    --engine "mysql://root@127.0.0.1:${ENGINE_PORT}" \
    --engine-instance "$instance" \
    ${EXTRA[@]+"${EXTRA[@]}"}
