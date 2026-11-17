# epoll vs io_uring — comparison

Status: preliminary, but covers echo ping-pong, connection churn, low-concurrency
latency percentiles and idle connection fan-out. CQ-overflow and worker-restart
soak cases are still not measured.

## Environment

- CPU: Intel Core i7-12700K, 20 logical CPUs (1 thread/core reported)
- Kernel: `5.15.0-139-generic` (io_uring 5.15, EXT_ARG present, multishot wake
  poll present)
- Rust: release profile (`lto = true`), nightly toolchain in this workspace
- Client and server: same machine, loopback

## Workloads

- `examples/echo.rs` server, 8 workers, 100 connections, 100-byte payload,
  5 seconds per run, 2 runs, 1 s warm-up.
- `echo`: persistent connections repeatedly echo one message.
- `churn`: each connection is opened, one message is echoed, then it is closed
  and the next connection starts.
- `latency`: 20 persistent connections, 100-byte payload, 3 seconds, 3 runs;
  the client records every RTT and reports p50/p95/p99.
- `idle`: 1000/5000 connections that are opened and held without sending
  anything; server CPU is sampled from `/proc/<pid>/stat`.
- The clients are always the **epoll** build
  (`--no-default-features --features "io_cancel io_timeout work_steal"`), so only
  the server backend varies.
- Epoll server and io_uring server are separate release builds.

## Throughput (median of 2 runs)

| backend | echo | churn |
|---|---:|---:|
| epoll | 1,022,832 req/s | 22,428 req/s |
| io_uring (v1) | 905,719 req/s | 22,110 req/s |
| delta | −11.5% | −1.4% |

A previous 3-run, 200-connection echo run showed the same shape: epoll
1,036,697 vs io_uring 915,809 req/s (−11.7%).

Raw data: [docs/bench/io_backend_compare.csv](bench/io_backend_compare.csv).

Reproduce:

```sh
WORKLOADS="echo churn" benches/io_backend_compare.sh 18083 8 100 5 2
WORKLOADS=latency    benches/io_backend_compare.sh 18084 8 20 3 3
```

## Latency (20 connections, 3 runs)

| backend | p50 | p95 | p99 |
|---|---:|---:|---:|
| epoll | 8 µs | 14–26 µs | 219–409 µs |
| io_uring v1 | 8 µs | 16–18 µs | 240–334 µs |

At low concurrency the two backends are effectively at parity: same median,
overlapping p95/p99 ranges, and both around 900–950k requests/s in these short
runs. The earlier script run with 20 connections also showed this variance
(epoll 880k, io_uring 741k in one pair of 2-run runs), so no strong single-run
claim should be made.

## Idle fan-out (server CPU, 3 s hold)

| connections | epoll CPU | io_uring CPU | delta |
|---:|---:|---:|---:|
| 1,000 | 0.150 s | 0.110 s | −27% |
| 5,000 | 0.490 s | 0.400 s | −18% |

The io_uring backend avoids per-connection reactor registration, and this is
the first workload where that shows up. The absolute numbers include TCP
handshake and handler/coroutine setup, so they are not a pure registration
cost; the difference grows only slowly with connection count in this range.

Manual measurement:

```sh
HZ=$(getconf CLK_TCK)
server -t 8 -p 18088 & pid=$!
t0=$(awk '{print $14+$15}' /proc/$pid/stat)
target/bench-epoll/release/examples/idle_client -c 5000 -t 8 -d 3 -a 127.0.0.1:18088
t1=$(awk '{print $14+$15}' /proc/$pid/stat)
awk -v a=$t0 -v b=$t1 -v hz=$HZ 'BEGIN {printf "%.3f s\n", (b-a)/hz}'
```

## Interpretation

- **Ping-pong throughput**: io_uring is ~11% slower. The epoll design usually
  gets the data in the first non-blocking `read`, then writes it, so an echo
  message costs at most two syscalls. The io_uring design submits one SQE and
  enters the kernel per blocked operation, then processes a CQE a round later,
  plus active-request bookkeeping. May's one-operation-per-coroutine model does
  not create the batches that normally make io_uring cheaper than a syscall per
  operation.
- **Connection churn**: at parity. The TCP handshake and coroutine/handler setup
  dominate over `epoll_ctl(ADD/DEL)` at this rate.
- **Low-concurrency latency**: at parity within run-to-run noise.
- **Idle fan-out**: io_uring uses less server CPU, consistent with skipping
  per-connection reactor registration.

The next optimisation should target batching rather than micro-tuning the
dispatch loop:

1. Defer submission and let the owning worker enter once with several SQEs.
2. Consider multishot accept/recv where the API allows it.
3. Re-run the matrix with larger connection counts and longer runs to reduce
   variance before drawing finer conclusions.

No conclusion should be drawn about workloads outside these four until the
remaining soak cases are measured.
