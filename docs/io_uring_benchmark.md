# epoll vs io_uring — comparison

Status: preliminary, covering echo ping-pong, connection churn, low-concurrency
latency percentiles and idle connection fan-out. The current io_uring build
uses **batched submission** (see below); earlier per-op-submit numbers are kept
where they help explain the change.

## Environment

- CPU: Intel Core i7-12700K, 20 logical CPUs (1 thread/core reported)
- Kernel: `5.15.0-139-generic` (io_uring 5.15, EXT_ARG present, multishot wake
  poll present)
- Rust: release profile (`lto = true`), nightly toolchain in this workspace
- Client and server: same machine, loopback

## Submission modes

- **v1 — per-operation submit**: each adapter pushed an SQE and immediately
  called `io_uring_enter`.
- **v2 — batched submit** (current): same-shard submissions are pushed into the
  ring and the owning worker enters once for everything queued in that
  event-loop round. Cross-thread submissions still enter immediately. A cancel
  of a queued request force-submits it before queueing `AsyncCancel`.

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

## Throughput (median of 2 runs, batched submission)

| backend | echo | churn |
|---|---:|---:|
| epoll | 1,035,489 req/s | 22,816 req/s |
| io_uring v2 | 994,586 req/s | 24,775 req/s |
| delta | −3.9% | +8.6% |

For reference, the v1 per-operation submit build measured epoll 1,022,832 vs
io_uring 905,719 on echo (−11.5%) and epoll 22,428 vs io_uring 22,110 on churn
(−1.4%).

Raw data: [docs/bench/io_backend_compare.csv](bench/io_backend_compare.csv).

Reproduce:

```sh
WORKLOADS="echo churn" benches/io_backend_compare.sh 18093 8 100 5 2
WORKLOADS=latency    benches/io_backend_compare.sh 18094 8 20 3 3
```

## Latency (20 connections, 3 runs, batched submission)

| backend | p50 | p95 | p99 | throughput |
|---|---:|---:|---:|---:|
| epoll | 8 µs | 15–27 µs | 368–411 µs | 890–955k req/s |
| io_uring v2 | 8 µs | 23–25 µs | 489–503 µs | 823–834k req/s |

The v1 per-operation build had lower tail latency (p99 240–334 µs, p95 16–18
µs) and roughly parity throughput. Batching trades some low-concurrency
latency for higher throughput: a queued request waits for the owning worker's
next event-loop round before the kernel sees it.

## Idle fan-out (server CPU, 3 s hold)

| connections | epoll CPU | io_uring CPU | delta |
|---:|---:|---:|---:|
| 1,000 | 0.180 s | 0.130 s | −28% |
| 5,000 | 0.560 s | 0.450 s | −20% |

The io_uring backend avoids per-connection reactor registration, and this is
the workload where that advantage is clearest. The absolute numbers include
TCP handshake and handler/coroutine setup, so they are not a pure registration
cost; an earlier v1 run measured 0.150/0.110 s at 1,000 and 0.490/0.400 s at
5,000.

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

- **Echo throughput**: batching reduced the gap from −11.5% to −3.9%. The
  remaining difference is mostly the epoll fast path: an echo message usually
  costs two direct syscalls, while io_uring still pays completion processing
  and an event-loop round.
- **Churn**: batching made io_uring faster (+8.6%); accept/connect submissions
  batch into one enter and there is no per-connection registration.
- **Low-concurrency latency**: p50 is identical, but batching widened p99
  (~500 µs vs ~400 µs) and lowered the 20-connection throughput. This is the
  direct cost of deferring submission to the next round.
- **Idle fan-out**: io_uring uses ~20–28% less server CPU.

A possible follow-up is an adaptive policy: submit immediately when the ring
is otherwise empty, and batch only when there is already queued work. That
would recover latency without giving up burst batching. Larger connection
counts and longer runs are also needed to reduce variance before finer claims.

No conclusion should be drawn about workloads outside these four.
