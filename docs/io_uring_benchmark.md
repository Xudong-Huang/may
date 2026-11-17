# epoll vs io_uring — first comparison

Status: preliminary. This is the tight ping-pong workload; churn, idle
fan-out and latency-percentile workloads described in the plan are not
implemented yet.

## Environment

- CPU: Intel Core i7-12700K, 20 logical CPUs (1 thread/core reported)
- Kernel: `5.15.0-139-generic` (io_uring 5.15, EXT_ARG present, multishot wake
  poll present)
- Rust: release profile (`lto = true`), nightly toolchain in this workspace
- Client and server: same machine, loopback

## Workload

- `examples/echo.rs` server, 8 workers, 200 persistent connections, 100-byte
  payload, 5 seconds per run, 3 runs, 1 s warm-up.
- The client is always the **epoll** build
  (`--no-default-features --features "io_cancel io_timeout work_steal"`), so only
  the server backend varies.
- Epoll server and io_uring server are separate release builds.

## Result (median of 3 runs)

| backend | median request/s | relative |
|---|---:|---:|
| epoll | 1,036,697 | — |
| io_uring (v1) | 915,809 | −11.7% |

Raw data: [docs/bench/io_backend_compare.csv](bench/io_backend_compare.csv).

Reproduce:

```sh
benches/io_backend_compare.sh 18081 8 200 5 3
```

## Interpretation

The io_uring backend is currently slower on this tight ping-pong echo workload.
Both backends perform the same application-level work per message, but:

- The epoll design usually gets the data in the first non-blocking `read`, then
  writes it, so an echo message costs at most two syscalls and often fewer
  scheduling round-trips.
- The io_uring design submits one SQE and enters the kernel per blocked
  operation (`io_uring_enter` with `min_complete = 0`), then processes a CQE
  round later. There is also the active-request bookkeeping and completion
  wakeup. May's one-operation-per-coroutine model does not create the batches
  that normally make io_uring cheaper than a syscall per operation.

This is the expected trade-off, not a correctness issue. It means the next
optimisations should target batching rather than micro-tuning the dispatch
loop:

1. Defer submission and let the owning worker enter once with several SQEs.
2. Consider multishot accept/recv where the API allows it.
3. Measure connection churn and idle fan-out, where io_uring's lack of
   `epoll_ctl(ADD/DEL)` should favour it.
4. Measure per-message latency percentiles; throughput alone under-reports the
   completion hop's effect on tail latency.

No conclusion should be drawn about workloads other than this loopback echo
case until those measurements are added.
