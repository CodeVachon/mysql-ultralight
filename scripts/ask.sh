#!/bin/bash
# Ask MySQL 8.3 and mysql-ultralight the same SQL, with the same client, and
# show both answers side by side.
#
#   scripts/ask.sh <file.sql>            compare rows
#   COLINFO=1 scripts/ask.sh <file.sql>  compare column metadata as well
#
# For settling one question now, where scripts/differential.sh is for keeping
# every settled question settled. Reach for this before working an issue: an
# issue body is prose somebody wrote once, and in this project the prose has
# been wrong often enough that measuring first is cheaper than implementing a
# premise twice. Several issues turned out already fixed, half fixed, or fixed
# in a way that made the remaining half smaller than the title suggested.
#
# Three traps this script exists to avoid, all of which produced a false
# "agree" before the guards below were added:
#
#   * a probe of only INSERT/UPDATE/DDL compares nothing, because the client
#     prints nothing for a statement with no result set. Ask ROW_COUNT() or
#     SELECT the table back. Two empty outputs are reported INCONCLUSIVE.
#   * a claim about a column's type, collation, or flags is invisible in the
#     rows. Use COLINFO=1, or the values agree and the metadata still differs.
#   * the client charset is pinned to utf8mb4 because the container's client
#     defaults to latin1, which reports latin1_swedish_ci for every string
#     column. #0074 in this repo was mis-measured for exactly that reason.
#
# The client runs inside the container for both servers, so a formatting
# difference between two client builds cannot read as a divergence. The engine
# is reached from the container as host.docker.internal.
set -uo pipefail
# COLINFO=1 adds --column-type-info, which is how a claim about a column's
# type, flags, or nullability gets checked rather than a claim about its rows.
COL=""
[ "${COLINFO:-}" = 1 ] && COL="--column-type-info"
REPO=$(cd "$(dirname "$0")/.." && pwd)
MYPORT=33320
CONTAINER=mysql-validate

start_mysql() {
    if ! docker ps --format '{{.Names}}' | grep -qx "$CONTAINER"; then
        docker run --rm -d -p $MYPORT:3306 -e MYSQL_ALLOW_EMPTY_PASSWORD=1 \
            --name "$CONTAINER" mysql:8.3 >/dev/null
        printf 'waiting for mysql:8.3 ' >&2
        until docker exec "$CONTAINER" mysqladmin ping --silent >/dev/null 2>&1; do
            printf . >&2; sleep 2
        done
        echo " up" >&2
    fi
}

start_engine() {
    # A fresh engine per file, so one probe cannot leave state for the next.
    "$REPO/target/release/mysql-ultralight" --port 0 --database probe > "$1.engine.log" 2>&1 &
    echo $! > "$1.engine.pid"
    for _ in $(seq 1 100); do
        p=$(grep -o 'port=[0-9]*' "$1.engine.log" 2>/dev/null | head -1 | cut -d= -f2)
        [ -n "${p:-}" ] && [ "$p" != 0 ] && { echo "$p"; return; }
        sleep 0.1
    done
    echo "ENGINE_FAILED" ; cat "$1.engine.log" >&2
}

file=$1
start_mysql
docker exec "$CONTAINER" mysql -uroot -e "DROP DATABASE IF EXISTS probe; CREATE DATABASE probe;" 2>/dev/null
eport=$(start_engine "$file")
[ "$eport" = "ENGINE_FAILED" ] && { echo "engine did not start"; exit 1; }

# --table for stable formatting; errors carry their code and SQLSTATE.
# The client lives in the container, so both servers are asked by the *same*
# client build -- otherwise a formatting difference reads as a divergence.
# The engine runs on the host, reachable from the container as
# host.docker.internal.
docker exec -i "$CONTAINER" mysql -h127.0.0.1 -uroot -Dprobe --default-character-set=utf8mb4 --table $COL --force \
    < "$file" > "$file.mysql.out" 2>&1
docker exec -i "$CONTAINER" mysql -hhost.docker.internal -P"$eport" -uroot -Dprobe --default-character-set=utf8mb4 --table $COL --force \
    < "$file" > "$file.engine.out" 2>&1
kill "$(cat "$file.engine.pid")" 2>/dev/null

# Two empty outputs are not agreement. The client prints nothing for a
# statement with no result set, so a probe made only of INSERT/UPDATE/DDL
# compares nothing at all and would otherwise read as a clean pass -- the
# exact shape of a test that looks like evidence and is not. Ask for
# ROW_COUNT() or SELECT the table back instead.
if [ ! -s "$file.mysql.out" ] && [ ! -s "$file.engine.out" ]; then
    echo "INCONCLUSIVE: both servers returned no output; this probe compares nothing"
elif diff -q "$file.mysql.out" "$file.engine.out" >/dev/null; then
    echo "AGREE"
else
    echo "DIVERGE"
    diff -u "$file.mysql.out" "$file.engine.out" | sed -e 's/^---.*/--- mysql 8.3/' -e 's/^+++.*/+++ engine/'
fi
