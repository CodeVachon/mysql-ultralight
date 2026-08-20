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
# Kept rather than discarded: a port already in use makes the engine exit, and
# without its output the harness only reports that it could not connect.
engine_log=$(mktemp)
./target/release/mysql-ultralight --port "$ENGINE_PORT" --database differential >"$engine_log" 2>&1 &
engine_pid=$!

# The engine is ready as soon as it accepts a connection; it has no warm-up.
ready=0
for _ in $(seq 1 100); do
    if (echo > "/dev/tcp/127.0.0.1/${ENGINE_PORT}") >/dev/null 2>&1; then ready=1; break; fi
    sleep 0.1
done
if [ "$ready" = 0 ]; then
    echo "the engine never accepted a connection on port ${ENGINE_PORT}:" >&2
    cat "$engine_log" >&2
    exit 2
fi

# `${EXTRA[@]}` on an empty array is an unbound variable under `set -u` in the
# bash macOS ships, so the expansion is guarded rather than the option dropped.
cargo run --release -q -p mysql-ultralight-differential -- \
    --reference "mysql://root@127.0.0.1:${REF_PORT}" \
    --engine "mysql://root@127.0.0.1:${ENGINE_PORT}" \
    ${EXTRA[@]+"${EXTRA[@]}"}
