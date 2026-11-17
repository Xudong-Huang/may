# epoll vs io_uring — comparison

Status: preliminary, covering echo ping-pong, connection churn, low-concurrency
latency percentiles and idle connection fan-out. The current io_uring build
uses **batched submission** (see below); earlier per-op-submit numbers are kept
where they help explain the change. An adaptive "submit the first operation of
each round immediately" policy was measured and rejected.

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
- **v3 — round-flag adaptive (rejected)**: the first same-shard submission
  after the worker woke entered immediately; the rest of that round's
  submissions were batched. A variant that submitted whenever the SQ was
  empty degenerated to v1, because each immediate submit emptied the queue
  again. The round-flag variant cost throughput without reducing tail latency,
  so v2 was kept (see Interpretation).

## Workloads

- `examples/echo.rs` server, 8 workers, 100 connections, 100-byte payload,
  5 seconds per run, 3 runs, 1 s warm-up.
- `echo`: persistent connections repeatedly echo one message.
- `churn`: each connection is opened, one message is echoed, then it is closed
  and the next connection starts.
- `latency`: 20 persistent connections, 100-byte payload, 3 seconds, 3 runs;
  the client records every RTT and reports p50/p95/p99.
- `idle`: 1000/5000/10000 connections that are opened and held without sending
  anything; server CPU is sampled from `/proc/<pid>/stat`.
- The clients are always the **epoll** build
  (`--no-default-features --features "io_cancel io_timeout work_steal"`), so only
  the server backend varies.
- Epoll server and io_uring server are separate release builds; the io_uring
  server is built with `--features io_uring` (the feature is opt-in).

## Throughput (median of 3 runs, batched submission)

| backend | echo | churn |
|---|---:|---:|
| epoll | 1,035,448 req/s | 24,736 req/s |
| io_uring v2 | 997,613 req/s | 24,763 req/s |
| delta | −3.7% | +0.1% |

Churn is reported as parity: epoll measured 22.3–24.9k and io_uring
22.0–24.8k across the three runs, so the median difference is inside the
run-to-run variance. Echo is stable for io_uring (992–999k) while epoll ranges
1,033–1,062k.

For reference, the v1 per-operation build measured epoll 1,022,832 vs
io_uring 905,719 on echo (−11.5%) and epoll 22,428 vs io_uring 22,110 on churn
(−1.4%) in an earlier session. In a back-to-back rerun on the same host, v1
measured 915,137 vs epoll 1,037,398 on echo and 21,993 vs 24,767 on churn; the
round-flag variant measured 973,375 and 22,034 respectively. This is why
churn deltas are not treated as a stable win in either direction.

Raw data: [docs/bench/io_backend_compare.csv](bench/io_backend_compare.csv).

Reproduce:

```sh
WORKLOADS="echo churn" benches/io_backend_compare.sh 18093 8 100 5 3
WORKLOADS=latency    benches/io_backend_compare.sh 18094 8 20 3 3
```

### 500 connections

The same echo/churn workload with 500 connections (three runs):

| backend | echo | churn |
|---|---:|---:|
| epoll | 1,081,301 req/s | 24,336 req/s |
| io_uring | 1,030,580 req/s | 21,741 req/s |
| delta | −4.7% | within variance |

Echo remains stable and the gap is slightly wider than at 100 connections.
Churn ranges overlap heavily (epoll 22.1–24.4k, io_uring 21.7–24.5k), so no
directional claim is made. Raw data:
[docs/bench/io_backend_compare_500.csv](bench/io_backend_compare_500.csv).

```sh
WORKLOADS="echo churn" benches/io_backend_compare.sh 18120 8 500 5 3
```

## Latency (20 connections, 3 runs, batched submission)

| backend | p50 | p95 | p99 | throughput |
|---|---:|---:|---:|---:|
| epoll | 8 µs | 12–22 µs | 134–314 µs | 907–948k req/s |
| io_uring v2 | 8 µs | 21–24 µs | 477–496 µs | 826–853k req/s |

A same-session v1 per-operation rebuild measured p50 7 µs, p95 21 µs and p99
470–479 µs. The earlier observation that batching widened the p99 tail
(489–503 µs versus 240–334 µs) did not reproduce: the tail is dominated by
wakeup/scheduling variance rather than by whether the SQE is submitted now or
at the next event-loop turn. The round-flag adaptive variant (p99 428–472 µs)
did not change that either. p50/p95 are close across backends; io_uring's
20-connection throughput is a few percent below epoll in this workload.

## Idle fan-out (server CPU, 3 s hold, median of 3 runs)

| connections | epoll CPU | io_uring CPU | delta |
|---:|---:|---:|---:|
| 1,000 | 0.150 s | 0.130 s | −13% |
| 5,000 | 0.480 s | 0.410 s | −15% |
| 10,000 | 0.780 s | 0.680 s | −13% |

The io_uring backend avoids per-connection reactor registration, and this is
the workload where that advantage is clearest. The absolute numbers include
TCP handshake and handler/coroutine setup, so they are not a pure registration
cost. Earlier single runs measured deltas of 20–28%; with three repeats per
point the ranges overlap more than that (for example 0.67–0.79 s versus
0.55–0.78 s at 10,000), which is why the conservative three-run medians are
reported. Raw data: [docs/bench/io_idle_cpu.csv](bench/io_idle_cpu.csv).

Manual measurement:

```sh
HZ=$(getconf CLK_TCK)
server -t 8 -p 18088 & pid=$!
t0=$(awk '{print $14+$15}' /proc/$pid/stat)
target/bench-epoll/release/examples/idle_client -c 5000 -t 8 -d 3 -a 127.0.0.1:18088
t1=$(awk '{print $14+$15}' /proc/$pid/stat)
awk -v a=$t0 -v b=$t1 -v hz=$HZ 'BEGIN {printf "%.3f s\n", (b-a)/hz}'
```

## Syscall profile (context only)

`strace -f -c` over a 2-second 100-connection echo run. Tracing inflates
latency and lowers throughput, so only the call counts are meaningful:

| backend | sendto | recvfrom | event wait | registration | accept4 |
|---|---:|---:|---:|---:|---:|
| epoll | 695,075 | 695,275 | 4,300 `epoll_wait` | 416 `epoll_ctl` | 234 |
| io_uring | 694,741 | 694,941 | 5,074 `io_uring_enter` | none | 208 |

Both backends served ~467k echoed requests almost entirely through the
nonblocking `sendto`/`recvfrom` fast path; only a small fraction needed a ring
submission. io_uring made slightly more enter calls than epoll made wait calls
and avoided the `epoll_ctl` registrations, but registration is small at 100
connections. The remaining echo difference is therefore event-loop and
completion processing, not syscall count.

## Interpretation

- **Echo throughput**: batching reduced the gap from −11.5% to −3.7% at 100
  connections and −4.7% at 500. The remaining difference is mostly the epoll
  fast path: an echo message usually costs two direct syscalls, while io_uring
  still pays completion processing and an event-loop round.
- **Churn**: batching moved churn to parity. The earlier +8.6% result did not
  reproduce; epoll churn itself varied 22.3–24.9k across three runs, so no
  stable win or loss is claimed.
- **Low-concurrency latency**: p50 is identical and p95 is close (io_uring
  21–24 µs, epoll 12–22 µs), but io_uring keeps a wider p99 tail
  (477–496 µs versus 134–314 µs for epoll) and a few percent lower
  20-connection throughput. Per-op submit (v1) and the round-flag adaptive
  variant measured the same p99 range in back-to-back runs, so this is not a
  batching artifact; the completion hop and wakeup/scheduling variance
  dominate at this scale.
- **Idle fan-out**: io_uring uses ~13–15% less server CPU in three-run
  medians; earlier single runs suggested 20–28%, but the measurement includes
  connection setup and varies run to run.

Adaptive submission was evaluated and rejected: it did not reduce the tail
latency it targeted and cost throughput (echo 973k round-flag / 915k v1 versus
998k batched), so batched submission stays. A more useful follow-up is reducing
completion-processing and fast-path syscall cost, plus longer CPU-pinned runs
to separate backend differences from run-to-run variance. Larger connection
counts, longer runs, and a syscall profile are still needed before finer
claims.

No conclusion should be drawn about workloads outside these four.
