# epoll vs io_uring — first comparison

Status: preliminary. This covers the tight ping-pong and connection-churn
workloads; idle fan-out and latency-percentile workloads described in the plan
are not implemented yet.

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
- The clients are always the **epoll** build
  (`--no-default-features --features "io_cancel io_timeout work_steal"`), so only
  the server backend varies.
- Epoll server and io_uring server are separate release builds.

## Result (median of 2 runs)

| backend | workload | median request/s | relative |
|---|---|---:|---:|
| epoll | echo | 1,022,832 | — |
| io_uring (v1) | echo | 905,719 | −11.5% |
| epoll | churn | 22,428 | — |
| io_uring (v1) | churn | 22,110 | −1.4% |

A previous 3-run, 200-connection echo run showed the same shape: epoll
1,036,697 vs io_uring 915,809 req/s (−11.7%).

Raw data: [docs/bench/io_backend_compare.csv](bench/io_backend_compare.csv).

Reproduce:

```sh
WORKLOADS="echo churn" benches/io_backend_compare.sh 18083 8 100 5 2
```

## Interpretation

io_uring is slower on the tight ping-pong echo workload and roughly at parity
on connection churn with the current implementation. Both backends perform the
same application-level work per message, but:

- The epoll design usually gets the data in the first non-blocking `read`, then
  writes it, so an echo message costs at most two syscalls and often fewer
  scheduling round-trips.
- The io_uring design submits one SQE and enters the kernel per blocked
  operation (`io_uring_enter` with `min_complete = 0`), then processes a CQE
  round later. There is also the active-request bookkeeping and completion
  wakeup. May's one-operation-per-coroutine model does not create the batches
  that normally make io_uring cheaper than a syscall per operation.
- Connection churn is dominated by the TCP handshake and process/coroutine
  setup, not by `epoll_ctl(ADD/DEL)`; the expected registration win did not
  show up at this rate.
- Idle fan-out may still favour io_uring because there is no per-connection
  reactor registration, but it has not been measured yet.

These are expected trade-offs, not correctness issues. The next optimisations
should target batching rather than micro-tuning the dispatch loop:

1. Defer submission and let the owning worker enter once with several SQEs.
2. Consider multishot accept/recv where the API allows it.
3. Measure idle fan-out and per-message latency percentiles; throughput alone
   under-reports the completion hop's effect on tail latency.

No conclusion should be drawn about workloads other than loopback echo and
churn until those measurements are added.
