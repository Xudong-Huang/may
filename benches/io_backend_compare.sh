#!/usr/bin/env bash
#
# Compare the Linux epoll and io_uring backends with the same echo client.
#
# The client is always built with the epoll backend so only the server backend
# varies. Results are written to stdout as a small Markdown table and as CSV.
#
# Usage:
#   benches/io_backend_compare.sh [port] [workers] [connections] [seconds] [runs]
#
# The io_uring server needs a kernel/container where io_uring is permitted.

set -euo pipefail

cd "$(dirname "$0")/.."

PORT="${1:-18080}"
WORKERS="${2:-8}"
CONNECTIONS="${3:-200}"
SECONDS_PER_RUN="${4:-8}"
RUNS="${5:-3}"
PAYLOAD=100
CLIENT_WORKERS="$WORKERS"
URING_TARGET="${URING_TARGET:-target}"
EPOLL_TARGET="${EPOLL_TARGET:-target/bench-epoll}"
CSV="${CSV:-/tmp/may_io_backend_compare.csv}"

log() { printf '%s\n' "$*" >&2; }

build() {
    log "building io_uring server (default features)"
    cargo build --release --example echo --target-dir "$URING_TARGET" >/dev/null

    log "building epoll server/client"
    cargo build --release --no-default-features \
        --features "io_cancel io_timeout work_steal" \
        --example echo --example echo_client --target-dir "$EPOLL_TARGET" >/dev/null
}

wait_for_port() {
    local port=$1
    for _ in $(seq 1 100); do
        if (echo >"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
            return 0
        fi
        sleep 0.05
    done
    return 1
}

run_once() {
    local backend=$1
    local server=$2
    local run=$3

    local server_log
    server_log=$(mktemp)
    "$server" -t "$WORKERS" -p "$PORT" >"$server_log" 2>&1 &
    local server_pid=$!
    # shellcheck disable=SC2064
    trap "kill $server_pid 2>/dev/null || true; rm -f $server_log" RETURN

    if ! wait_for_port "$PORT"; then
        log "server $backend did not start; log:"
        cat "$server_log" >&2
        return 1
    fi

    # Warm up, then measure.
    "$EPOLL_TARGET/release/examples/echo_client" \
        -c "$CONNECTIONS" -l "$PAYLOAD" -t "$CLIENT_WORKERS" \
        -d 1 -a "127.0.0.1:$PORT" >/dev/null 2>&1 || true

    local output
    output=$("$EPOLL_TARGET/release/examples/echo_client" \
        -c "$CONNECTIONS" -l "$PAYLOAD" -t "$CLIENT_WORKERS" \
        -d "$SECONDS_PER_RUN" -a "127.0.0.1:$PORT" 2>&1 | tail -n 3)
    local speed
    speed=$(printf '%s\n' "$output" | sed -n 's/.*Speed: \([0-9]*\) request\/sec.*/\1/p' | head -n1)

    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
    trap - RETURN
    rm -f "$server_log"

    if [[ -z "$speed" ]]; then
        log "could not parse client output for $backend run $run:"
        printf '%s\n' "$output" >&2
        return 1
    fi
    printf '%s,%s,%s\n' "$backend" "$run" "$speed" >>"$CSV"
    printf '%s\n' "$speed"
}

build
: >"$CSV"

log "workload: workers=$WORKERS connections=$CONNECTIONS payload=$PAYLOAD time=${SECONDS_PER_RUN}s runs=$RUNS"

for backend in epoll io_uring; do
    if [[ "$backend" == epoll ]]; then
        server="$EPOLL_TARGET/release/examples/echo"
    else
        server="$URING_TARGET/release/examples/echo"
    fi
    for run in $(seq 1 "$RUNS"); do
        speed=$(run_once "$backend" "$server" "$run")
        log "$backend run $run: $speed req/s"
    done
done

printf '\n| backend | runs | median req/s |\n|---|---:|---:|\n'
for backend in epoll io_uring; do
    median=$(
        awk -F, -v backend="$backend" '$1 == backend { print $3 }' "$CSV" |
            sort -n |
            awk '{ a[NR] = $1 } END { if (NR % 2) print a[(NR + 1) / 2]; else print int((a[NR / 2] + a[NR / 2 + 1]) / 2) }'
    )
    printf '| %s | %s | %s |\n' "$backend" "$RUNS" "$median"
done
printf '\nraw data: %s\n' "$CSV"
