# Linux `io_uring` backend — implementation plan

Status: **M0–M9 implemented** — completion backend active; batch same-shard
submission landed. Per-op and round-flag adaptive submit policies were measured
and rejected (no tail-latency gain, less throughput); batched submission stays.

## 1. Goal and scope

Add a completion-based `io_uring` I/O backend for Linux, mirroring the Windows
IOCP backend, while keeping the existing Linux `epoll` backend as the default.

**Goals**

- Cargo feature `io_uring`, enabled by default **on this development branch**.
  Before merge, decide whether it stays on by default or returns to opt-in
  (`--no-default-features --features io_cancel,io_timeout,work_steal` restores
  the epoll-only build locally).
- Full behavioural parity for the existing I/O API:
  - `may::net::{TcpListener, TcpStream, UdpSocket}`
  - `may::os::unix::net::{UnixListener, UnixStream, UnixDatagram}`
  - `may::io::CoIo` and `may::io::{SplitIo, SplitReader, SplitWriter}`
  - `io_timeout` read/write timeouts, `io_cancel` coroutine cancellation
  - `may::io::WaitIo` / `WaitIoWaker`
- One ring per scheduler worker thread; no extra OS threads.
- A reproducible benchmark comparing the epoll backend against the `io_uring`
  backend on the same machine.

**Non-goals for the first version**

- Not making `io_uring` the release default (the branch default is temporary;
  see section 3).
- Runtime backend switching or automatic fallback to epoll (see open question 1).
- `SQPOLL`, `IOPOLL`, registered files/buffers, multishot recv/accept, zero-copy,
  or adaptive submit policies beyond the owner-round batching. These are
  further follow-up optimisations (section 7).
- New filesystem APIs. `CoIo` keeps working on file fds through
  `IORING_OP_READ/WRITE`, but there is no new fs module.

## 2. Why this is a separate `sys` backend, not a tweak to `epoll`

The two models differ structurally:

- **epoll backend**: `IoData` is per-fd. It stores `io_flag` and a `wait` slot;
  adapters first try the non-blocking syscall, then register readiness and retry
  after wakeup.
- **IOCP backend**: `EventData` is per *request*. The adapter submits an
  overlapped operation and completion status resumes the coroutine.
- **io_uring**: completion-based like IOCP, but an SQE owns raw pointers to the
  operation's buffer/address structures until the CQE arrives. A per-request
  state (`EventData`) and a per-worker completion ring are required; per-fd
  readiness state (`io_flag`) is meaningless.

Therefore the cleanest split is a second `sys` implementation tree, selected at
compile time, not a conditional inside the epoll selector.

## 3. Feature gating and module layout

### Cargo

```toml
[target.'cfg(target_os = "linux")'.dependencies]
io-uring = { version = "0.7.16", optional = true }
rcu_cell = { version = "1.2.5", optional = true }

[features]
io_uring = ["dep:io-uring", "dep:rcu_cell"]
```

`rcu_cell` provides the lock-free active-request slot (see section 4.2). It
requires Rust 1.84 and 64-bit targets; both apply to the io_uring backend's
supported Linux targets.

- Feature is additive. On this branch it is part of `default`, so `cargo
  check`/`cargo test` compile the ring layer; Linux runtime still uses epoll
  until M4 switches `sys`. The epoll-only path remains reachable with
  `--no-default-features --features io_cancel,io_timeout,work_steal`.
- Enabling the feature on a non-Linux target is a documented no-op rather than a
  `compile_error!`, so `--all-features` keeps working on Windows/macOS CI. All
  code/gates use `cfg(all(target_os = "linux", feature = "io_uring"))`.
- `io-uring` 0.7.16 declares `rust-version = 1.63`, so no nightly requirement is
  introduced for the feature itself.

### Selection in `src/io/mod.rs`

```rust
#[cfg(all(unix, not(all(target_os = "linux", feature = "io_uring"))))]
#[path = "sys/unix/mod.rs"]
pub(crate) mod sys;

#[cfg(all(target_os = "linux", feature = "io_uring"))]
#[path = "sys/linux_uring/mod.rs"]
pub(crate) mod sys;
```

The new tree lives under `src/io/sys/linux_uring/`: a top-level `sys` backend
peer to `sys/unix` (epoll/kqueue) and `sys/windows` (IOCP), with its logical
module root at `io::sys`. The temporary epoll bridge used during M1–M3 has been
removed; the tree now exports the real `EventData`, `Selector`, adapters and
`WaitIo` implementation.

### New files

| File | Responsibility |
|---|---|
| `linux_uring/mod.rs` | module glue, `submit_co!`, `add_socket`/`mod_socket`, re-exports |
| `linux_uring/event.rs` | `EventData`, `IoData`, `IoState` (`RcuWeak` active slot), cancel reasons, timers, `co_io_result`, shard selection |
| `linux_uring/ring.rs` | one `IoUring` per worker; SQE push+submit; `io_uring_enter` wait; eventfd wakeup `PollAdd`; CQ drain; thread-safety wrapper |
| `linux_uring/selector.rs` | sharded `Selector`: `select`, `submit`, `begin_request`, `cancel_request`, completion dispatch, io timers |
| `linux_uring/co_io.rs` | `CoIo<T>` (same public surface as today, completion-based readers/writers) |
| `linux_uring/cancel.rs` | `CancelIoData` / `CancelIoImpl` using `IORING_OP_ASYNC_CANCEL` |
| `linux_uring/wait_io.rs` | `WaitIo` / `WaitIoWaker` using `IORING_OP_POLL_ADD` |
| `linux_uring/net/*.rs` | the 12 adapters currently in `sys/unix/net/` (TCP, UDP, Unix-domain, read/write/peek/vectored/connect/accept) |

Shared, backend-neutral helpers (`co_io_err`, `split_io`, `thread`, `event_loop`)
are untouched. Small helpers duplicated from the Windows backend (for example a
completion result helper) are kept local to the backend; sharing across
platforms is not worth the abstraction.

## 4. Core design

### 4.1 Ring layer (`linux_uring/ring.rs`)

- Per worker shard:
  - `IoUring::builder().dontfork().build(entries)` where `entries` is
    `Config::get_io_uring_ring_entries()` (default 1024, power-of-two
    normalised) and the CQ is built with `setup_cqsize(2 × entries)`.
- A `nix::sys::eventfd::EventFd` for cross-thread wakeups, watched by a
  **multishot** `IORING_OP_POLL_ADD` (Linux 5.13+), so the poll stays registered
  across wakeups instead of being re-armed every round. If the kernel rejects
  the multishot arm (`-EINVAL` CQE, Linux 5.11–5.12), the ring permanently falls
  back to one-shot `PollAdd` re-arming. Every `wakeup` writes the eventfd (the
  scheduler already coalesces scheduler wakeups); the owner clears the counter
  after a wake CQE. A multishot CQE with `IORING_CQE_F_MORE` keeps the poll
  armed; a CQE without it means the poll ended and the next wait re-arms.
- `user_data` encoding:
  - `Arc<EventData>` pointer for real requests (aligned, never 0/1/2),
  - tag `1` for the wakeup poll,
  - tag `2` for `AsyncCancel` acknowledgement CQEs.
- Submission:
  - `push(entry)` + `submit()` (one `io_uring_enter` with `min_complete = 0`) in
    a shard-local `Mutex` protecting SQE mutation. The submit syscall does not
    block, so the lock is held only briefly.
  - Retry on `SQ` full (submit pending entries, then push again) and on `EINTR`.
  - On a submission error the adapter resumes the coroutine with that error
    through a Windows-style `co_try!`.
- Waiting (owner thread only):
  - Drain CQEs; if none, call `submit_with_args(1, SubmitArgs::timespec(...))`
    with the remaining timeout, then drain.
  - Treat `ETIME`/`ETIMEDOUT` as "no events"; retry `EINTR` with the remaining
    timeout, centred on `now()` (same rule as `select_loop.rs`).
- Thread-safety:
  - The `io-uring` crate declares `IoUring` `Send + Sync`, but its
    `SubmissionQueue` and `Submitter` are single-owner values whose cached
    head/tail must not be used concurrently. The shard mutex serialises SQE
    mutation (`submission_shared` + `push` + queue drop); the owner thread alone
    consumes the CQ; `io_uring_enter` may be called concurrently by producers
    and the owner, as the kernel supports multiple enterers. The invariants are
    documented on the wrapper.
  - Request shard selection: coroutine context uses the current worker id
    (`WORKER_ID`); plain-thread context (the proxy-coroutine path) hashes the
    fd, like `epoll.rs` does. `EventData` stores its shard so timer arming and
    cancellation agree with the ring that will deliver the CQE.
  - CQ consumption and `run_coroutine`/`schedule` are performed with no ring
    lock held, so a coroutine resumed during dispatch may submit a new request
    without deadlocking.

### 4.2 Request state (`EventData`) and `IoData`

Mirrors `sys/windows/iocp.rs`:

```rust
#[repr(C)]
pub struct EventData {
    pub fd: RawFd,                    // for diagnostics / accept plumbing
    pub shard: usize,                 // ring and timer shard
    io_state: Arc<IoState>,           // active slot / WaitIo waker state
    result: AtomicIsize,              // CQE result (bytes or -errno)
    pub timer: AtomicOption<TimerHandle<Arc<EventData>>>,
    co: AtomicOption<CoroutineImpl>,
    submitted: AtomicBool,            // timer/cancel race bookkeeping
    timed_out: AtomicBool,
    cancel_reason: AtomicU8,          // None / TimedOut / UserCancel / ConcurrentWait
}
```

- `Arc<EventData>` is held by the adapter frame (protected by `DelayDrop` or by
  a local clone, depending on the adapter), by the timer entry and by
  `CancelIoData`. The completion handler dereferences the pointer only *before*
  resuming the coroutine; after `co.take()` and the timer/result/error updates
  it never touches `EventData` again. This is the same lifetime discipline as
  the IOCP backend.
- `IoData` becomes thin: fd, shard, and `Arc<IoState>`. `IoState` holds the
  active-request slot (`rcu_cell::RcuWeak<EventData>`) used for concurrent-wait
  detection and the `WaitIo` waker state. `reset()` returns 0, `Drop` is a
  no-op. There is no kernel registration, no `io_flag`, no `publish_co`.
  `RcuWeak` gives wait-free reads and lockless writes, so submission and
  completion never block each other on this slot; the slot holds only a `Weak`
  (no cycle). Clearing a completed request uses `RcuWeak::compare_exchange`
  (rcu_cell 1.2.5), so it atomically removes only its own registration and can
  never clear a newer wait.
- Each `EventData` keeps a clone of `IoData`'s `Arc<IoState>`, so a completion
  can clear the active slot when it still points at the completing request.
  A second slow-path wait swaps in its own request, marks the previous one
  `ConcurrentWait`, and submits `AsyncCancel`; the previous waiter is resumed
  only by its own CQE, with a `concurrent io wait` error.
- Buffers, `msghdr`, `iovec`, `sockaddr` structures, and accepted-stream state
  live in the adapter's stack frame until the CQE. As in the IOCP adapters, a
  concurrent resume must not drop that frame while `subscribe` is still running:
  adapters either hold a `DelayDrop` guard or clone the `Arc<EventData>` locally
  before submitting, and never touch `self` after the point where the frame may
  be gone. The kernel owns raw pointers to the buffers until the CQE.
- `EventData` will need explicit `Send + Sync` impls (as the IOCP `EventData`
  does): the completion handler only reads CQE state and atomics, the timer and
  cancel paths only touch the atomic slots, and the coroutine owns the buffer
  pointers until the CQE.
- `user_data` is `Arc::as_ptr(&event_data) as u64`; CQE processing is exactly
  one-shot per request.

### 4.3 Selector round (`select`)

Ordered like the Windows selector:

1. Publish `sleep_until` for io-timer wakeup coalescing.
2. Drain up to `IO_POLLS_MAX` CQEs into `events` (copy into the backend-neutral
   `SysEvent` struct), advancing CQ head.
3. Dispatch each CQE:
   - wakeup tag: read the eventfd, clear the outstanding-poll bit when the poll
     ended, mark re-arm;
   - cancel tag: debug-log unexpected results, ignore;
   - request: read `result`; map negative results; handle `-ECANCELED` using
     `cancel_reason`; take `co`; clear timer; store result; schedule/resume
     (`schedule_with_id` under `work_steal`, inline otherwise, matching
     `iocp.rs`).
4. Re-arm the eventfd `PollAdd` if a wakeup was consumed.
5. Run due io timers (`timer_list.schedule_timer`); timeout handler only submits
   `AsyncCancel`, it never resumes the coroutine directly.
6. `scheduler.run_queued_tasks(id)`.
7. Recompute the relative `next_expire` after the drain.
8. Return `next_expire`.

The event loop's existing `disarm_wakeup` / `has_local_tasks` contract is
unchanged, so scheduler behaviour stays identical.

### 4.4 Operation mapping

| `may` adapter | io_uring opcode | completion result |
|---|---|---|
| `SocketRead` | `Read` | bytes (works on sockets and file fds, matching `CoIo`) |
| `SocketWrite` | `Write` | bytes |
| `SocketPeek` | `Recv` with `MSG_PEEK` | bytes |
| `SocketWriteVectored` | `Writev` | bytes |
| `TcpStreamConnect` | `Connect` | 0 / negative errno |
| `TcpListenerAccept` | `Accept` | new fd |
| `UdpRecvFrom` | `RecvMsg` with `msghdr` + `sockaddr_storage` | bytes; address parsed in `done()` |
| `UdpSendTo` | `SendMsg` | bytes |
| `UnixStreamConnect` | `Connect` with `sockaddr_un` | 0 |
| `UnixListenerAccept` | `Accept` | new fd; peer address via `peer_addr()` |
| `UnixRecvFrom` | `RecvMsg` with `MSG_PEEK`, then `recv_from` in `done()` | bytes + std address |
| `UnixSendTo` | `SendMsg` with `sockaddr_un` | bytes |
| `WaitIo` | `PollAdd(POLLIN|POLLOUT)` | revents mask |
| timeout / cancel | `AsyncCancel` | cancel ack; original request returns `-ECANCELED` |

Notes:

- `TcpStream::connect` / `UnixStream::connect` currently call
  `check_connected()` first. For `io_uring` the uring adapter's
  `check_connected()` returns `Ok(false)` without touching the socket, so the
  ring performs the connect. This avoids submitting `Connect` on a socket
  already in `EINPROGRESS`.
- `Accept` is submitted without an address buffer; `done()` obtains the peer
  address from the accepted stream (`peer_addr()`). That costs one syscall per
  accept but avoids reconstructing `std::os::unix::net::SocketAddr`, which has
  no public raw constructor. This is benchmarked and can be optimised later.
- `UnixRecvFrom` uses a peek-style completion because a std Unix `SocketAddr`
  cannot be built for unnamed senders. After the peek completes, `done()`
  consumes the datagram with the existing std `recv_from`, which is guaranteed
  ready. Only this Unix-datagram path pays the extra syscall; the TCP/UDP paths
  parse addresses directly.
- Fast paths in `net/tcp.rs`, `net/udp.rs`, `os/unix/net.rs` (non-blocking
  syscall before creating the adapter) stay in place. This preserves current
  semantics and avoids a completion round-trip for already-ready data. The
  benchmark notes this so the comparison is interpreted correctly.

### 4.5 Timeouts (`io_timeout`)

- Per-shard `TimeOutList<Arc<EventData>>` and `add_io_timer(&Arc<EventData>, dur)`,
  exactly as the IOCP backend.
- The adapter arms the timer **before** submission, stores `co`, submits, and
  then calls `cancel_if_timed_out()`. If the timer fired first it could not
  cancel a request that was not submitted; the post-submission check finishes
  the cancellation.
- `timeout_handler`:
  1. set `cancel_reason = TimedOut` with a `SeqCst` fence,
  2. disarm the timer,
  3. if `submitted`, submit `AsyncCancel`.
  It never resumes the coroutine; the original request's `-ECANCELED` CQE
  resumes it with `TimedOut`.
- If the operation completed successfully before the cancel was processed, the
  success result wins (same race rule as IOCP).
- `next_expire`, `sleep_until` wakeup coalescing, and “wake only for an earlier
  head timer” follow `iocp.rs`/`select_loop.rs`.

### 4.6 Cancellation (`io_cancel`)

- `CancelIoImpl` stores `CancelIoData { ev_data: Arc<EventData> }`, like IOCP.
- `CancelIoData::cancel()` submits `AsyncCancel` for the request's `user_data`.
  A successful submission means the cancellation is handled: the coroutine is
  resumed only by the original request's completion, so the kernel can no longer
  touch the buffer.
- If submission fails with `ENOENT` (request already completed) the completion
  handler resumes the coroutine normally; if it fails otherwise, `cancel()`
  returns `false` and the existing fallback finds no `co` slot for completion
  adapters, so **no early resume** happens. This is the key safety difference
  from the readiness backend, where cancel resumes the coroutine directly.
- The adapter registers `CancelIoData` after submission and re-checks the
  cancel bit, covering a cancel that landed before registration.
- `yield_back` still raises the `Cancel` panic; the completion handler must set
  no I/O error for user cancellation, so the panic path is unchanged.

### 4.7 `WaitIo` / `WaitIoWaker`

- `wait_io` submits a one-shot `PollAdd(POLLIN|POLLOUT)` on the fd and yields;
  the CQE's revents mask is returned.
- `WaitIoWaker::wakeup` marks a wake request and submits `AsyncCancel` for the
  outstanding poll; the poll's CQE resumes the waiter, which returns the same
  `WAKE_FLAG` bit the readiness backend uses.
- The per-wait `Arc<EventData>` is published into the thin `IoData`, so a stale
  waker cannot resume a later wait.

### 4.8 Public wrappers and split I/O

- `IoData::reset()` is a no-op returning 0; fast paths keep calling it.
- `mod_socket` is a no-op; `SplitIo` continues to `try_clone` the fd. Under
  `io_uring`, read and write halves simply submit their own operations; no
  interest mask exists.
- `TcpStream::try_clone` / `UdpSocket::try_clone` / `UnixStream::try_clone`
  continue to create a new fd and a fresh `IoData`; no IOCP-style registration
  quirk is needed.

### 4.9 Error mapping and edge cases

- CQE `res < 0` → `io::Error::from_raw_os_error(-res)`, consistent with
  `from_nix_error`.
- `-ECANCELED` + `TimedOut` → `TimedOut("timeout")`;
- `-ECANCELED` + `UserCancel` → no error; `yield_back` raises `Cancel`.
- Unknown `-ECANCELED` → `Interrupted("operation canceled")`.
- Collection move/close before CQE: the `Arc` held by the wait source (plus the
  delay guard) keeps `EventData` alive; no per-fd epoll DEL hazard exists.
- `AsyncCancel` CQEs and wakeup CQEs are filtered by tag and never resumed.
- CQ overflow: rely on `IORING_FEAT_NODROP` (kernel ≥ 5.5) and the crate's
  overflow flush; `select` drains all pending CQEs on the following round.
- Unsupported kernel: ring creation fails with a clear error from
  `EventLoop::new`. Tests skip when ring creation is not permitted.
- Minimum kernel: **5.11**, required by `IORING_FEAT_EXT_ARG` for the timed
  `io_uring_enter` wait; the wake poll uses the 5.13 multishot form when
  available and falls back to one-shot re-arming otherwise. Ring creation
  returns `Unsupported` when EXT_ARG is missing. Test target: 5.15.

## 5. Milestones (review checkpoints)

Each milestone stops for review. Intermediate milestones that build the tree
before the backend is selected are marked as such; the first end-to-end runnable
point is M4.

### M0 — scaffolding

- `Cargo.toml` feature and optional Linux dependency.
- Plan status update; user-facing docs are deferred to M8.
- No behavioural change; `cargo check` (default) and
  `cargo check --features io_uring` both green.
- **Review:** dependency/feature naming, non-Linux no-op policy.

### M1 — ring layer

- `linux_uring/ring.rs`: ring creation (`Config::get_io_uring_ring_entries`),
  SQE push + submit, enter/wait with timeout, eventfd + `PollAdd` wakeup,
  CQ drain, tags, `EINTR`/SQ-full handling.
- `config.rs`: `set_io_uring_ring_entries` / `get_io_uring_ring_entries`
  (Linux + feature only), with invalid-value handling.
- Unit tests: submit/complete a `Nop` or `Read` on a pipe, wakeup round-trip,
  timeout round-trip, thread-safety smoke test (submit from two threads).
- **Status:** implemented and unit-tested; committed as `104d17c`.
- **Review:** unsafe/thread-safety rationale, locking, error handling.

### M2 — request state and selector core

- `linux_uring/event.rs`: `EventData`, `IoData`, `IoState`, cancellation
  reasons, per-request timer handle, `co_io_result`, shard selection.
- `linux_uring/selector.rs`: sharded selector, `select`, completion dispatch,
  timer list, `sleep_until`, wakeup, `begin_request` concurrent-wait detection,
  `cancel_request`.
- `co_io.rs` landed with the M4 switch (it implements the crate's active
  `AsIoData`, which was not available while the epoll bridge was selected).
- **Review:** `select` ordering, completion lifetime, timer integration.
- **Status:** implemented and unit-tested; committed as `27af31f`.
- Compile state after M4: the bridge is removed and these modules are the
  active `sys` implementation; their adapters and end-to-end tests are part of
  the M4 diff.

### M3 — adapters (sub-commits, reviewed individually)

1. TCP: `SocketRead`, `SocketWrite`, `SocketPeek`, `SocketWriteVectored`,
   `TcpListenerAccept`, `TcpStreamConnect`.
2. UDP: `UdpRecvFrom`, `UdpSendTo`.
3. Unix-domain: `UnixListenerAccept`, `UnixStreamConnect`, `UnixRecvFrom`,
   `UnixSendTo`.
4. `mod.rs` glue: `submit_co!`, `add_socket`/`mod_socket`, accepted-fd
   plumbing, address helpers.
- Each adapter mirrors the IOCP adapter shape: `DelayDrop` or a local `Arc`
  clone, timer before submit, `co.store`, submit, cancel registration.
- **Status:** implemented (all 12 adapters plus `CoIo`); reviewed as part of
  the M4 switch diff.
- **Review:** buffer/pointer lifetimes, fast-path interaction, error paths.

### M4 — switch backend and first green end-to-end

- Select the uring tree in `io/mod.rs` when the feature is on.
- `net/tcp.rs` / `os/unix/net.rs`: uring `check_connected()` no-op; verify all
  `#[cfg(unix)]` blocks still compile (no `epoll`-only assumptions).
- Smoke tests: TCP echo, UDP echo, Unix stream/datagram, `CoIo` stdio/file,
  `SplitIo`, plain-thread I/O.
- **Status:** implemented — the backend is active on the branch and the
  epoll-only build still compiles via `--no-default-features`.
- **Review:** full diff and live test output; this is the first runnable point.

### M5 — timeouts

- Per-request timers, post-submission `AsyncCancel`, timeout-handler
  `AsyncCancel`; the adapter arms the timer before submission and
  `Selector::submit` finishes a cancellation that raced the submission.
- Tests: read timeout, zero timer, timeout vs completion; starvation tests
  still to add.
- **Status:** implemented; TCP read-timeout, sub-millisecond timeout and
  zero-timer tests pass, and `tests/io_uring_stress.rs` covers single-worker
  timer starvation with cancellation.
- **Review:** race analysis, timer shard mapping.

### M6 — cancellation

- `linux_uring/cancel.rs`, adapter registration, post-submission re-check.
- Tests: cancel waiting accept/read, cancel during submit, repeated/concurrent
  cancel, cancel stress, no buffer use-after-free (run under ASAN/Miri where
  feasible; Miri cannot drive io_uring, so use deliberate stress + `valgrind`).
- **Status:** implemented; `tests/cancel_stress.rs` passes in debug and release.
  Dedicated io_uring cancel tests are part of the M4 test file.
- **Review:** safe resume ordering (CQE before resume).

### M7 — stability and `WaitIo`

- `WaitIo`/`WaitIoWaker` via `PollAdd`; waker races.
- Wakeup coalescing, CQ overflow, high-connection-count soak, drop-while-pending
  stress, worker panic/restart behaviour.
- `cargo test --features io_uring` and feature-combination matrix.
- **Status:** `WaitIo` and the waker race fix are implemented and stress-tested;
  `tests/io_uring_stress.rs` adds single-worker timer-starvation and
  connection-soak scenarios, a ring unit test covers completion-queue
  overflow, and a scheduler unit test injects event-loop panics and verifies
  the worker restart path keeps coroutine scheduling and io_uring I/O alive.
- **Review:** stress-test logs and any residual unsafe invariants.

### M8 — docs and CI

- README/docs: feature usage, kernel requirement, unsupported-kernel behaviour,
  fast-path caveat.
- CI: `cargo check --features io_uring` in lints; a Linux test job that skips
  when io_uring is not permitted.
- **Status:** README and `src/io/README.txt` updated. CI now keeps the general
  matrix on the epoll feature set (hosted runners may block io_uring) and adds a
  dedicated Linux `io_uring` job that runs `tests/io_uring.rs` and
  `tests/io_uring_stress.rs`; those tests skip themselves when the runner
  rejects `io_uring_setup`. Lints also cover the epoll-only feature set.
- **Review:** docs wording, CI expectations.

### M9 — benchmarks

- Harness and results (section 6), committed as
  `docs/io_uring_benchmark.md` + script.
- **Status:** `benches/io_backend_compare.sh`, `latency_client` and
  `idle_client` added. Batched same-shard submission (v2) improved echo from
  −11.5% to −3.7%; churn is at parity within run-to-run variance (epoll itself
  varied 22.3–24.9k in a three-run set). Low-concurrency p99 stayed at
  ~480–500 µs for per-op, round-flag adaptive and batched submission alike, so
  it is not a batching artifact. Idle fan-out server CPU is 20–28% lower.
  Results, a context-only `strace` syscall profile and caveats are in
  `docs/io_uring_benchmark.md`; CQ-overflow and worker-restart tests live in
  M7. Adaptive submit policies were rejected; batched submission stays.
- **Review:** methodology, raw data, interpretation, follow-up list.

## 6. Test plan

### Unit tests (inside the uring tree)

- ring submit/complete, wakeup, timeout, `EINTR` and SQ-full paths;
- selector timer/wakeup coalescing with multiple shards;
- `user_data` tag filtering; cancel before/after submit;
- completion result mapping.

### Integration tests (`tests/io_uring.rs`, gated by
`cfg(all(target_os = "linux", feature = "io_uring"))`)

- TCP: connect success/refused, accept, read/write, peek, vectored write,
  half-close/EOF, `try_clone`, `split`, timeouts;
- UDP: connected `send/recv`, unconnected `send_to/recv_from` including address
  round-trip, broadcast/multicast setters (unchanged syscalls);
- Unix domain: stream connect/accept, datagram `send_to/recv_from` including
  unnamed sender and pathname addresses;
- `CoIo<File/Pipe>` read/write; stdio smoke if the environment allows;
- plain-thread context: call `TcpStream::read` from a non-coroutine thread;
- cancellation: the `tests/cancel_stress.rs` scenarios plus io-specific cancel;
- timeout: short timeout behind a longer one on the same shard; zero timeout;
- concurrent waits: the completion backend has per-request state, so it cannot
  detect two coroutines waiting on the same object the way the readiness backend
  does (IOCP has the same limitation). The test verifies no deadlock or memory
  unsafety and records the behavioural difference in docs; an optional per
  `IoData` busy flag is open question 8.

Every test begins with a `ring_supported()` guard that treats `EPERM`,
`ENOSYS`, `EOPNOTSUPP`, and disabled `io_uring` as a skip, so CI kernels without
io_uring do not produce false failures.

### Feature matrix

```
cargo test --features io_uring
cargo test --features io_uring --no-default-features
cargo test --features "io_uring io_cancel"
cargo test --features "io_uring io_timeout"
cargo test --features "io_uring io_cancel io_timeout work_steal"
```

(`io_timeout`/`io_cancel` are default features, so the important extra matrix is
the no-default combination; all gates must compile cleanly.)

### Tooling

- `cargo fmt --check`, `cargo clippy -- -D warnings` for feature and
  no-default-features builds.
- Nightly `cargo test --release` for the full suite.
- Optional: valgrind/ASAN runs for the cancel/timeout/drop stress tests.

## 7. Benchmark plan (epoll vs io_uring)

### Harness

- One client binary (epoll build) drives both server builds, so only the server
  backend varies.
- Builds:
  - epoll: `cargo build --release --example echo`
  - uring: `cargo build --release --features io_uring --example echo --target-dir target/uring`
- Script `benches/io_backend_compare.sh`:
  - starts the server, waits for the port, runs a 2 s warm-up, then N rounds per
    workload, kills the server, and writes CSV + a Markdown summary.
  - records the median of 3–5 rounds and server CPU (`/usr/bin/time -f '%P'`).
- Reuse `examples/echo.rs` / `examples/echo_client.rs` for throughput. Add
  small examples if the existing client cannot report the needed metric:
  - `examples/connect_churn.rs` — repeated connect / one echo / close;
  - `examples/latency_client.rs` — RTT p50/p95/p99 over persistent connections;
  - optional `--idle` mode — N idle connections plus a small active set.

### Workloads

1. **Ping-pong throughput**: 100-byte payload, connections 100, workers 1/4,
   10 s. Existing `echo` + `echo_client`.
2. **Low-concurrency latency**: 1–8 persistent connections, RTT percentiles.
3. **Connection churn**: short-lived connect/echo/close, measures
   `epoll_ctl(ADD/DEL)` versus no registration in io_uring.
4. **Idle fan-out**: 1,000–10,000 idle connections plus a few active ones,
   measures per-connection registration overhead and memory.
5. **Accept/connect cost** isolated if churn is noisy.
6. Optional syscall profile: `strace -c` / `perf stat` for a 5 s run, reported as
   context (not a headline metric).

### Reporting and interpretation

- Same host, loopback, fixed client/server CPU sets where possible; report CPU
  model, kernel, worker counts, and payload.
- Report median request/s, p50/p99 RTT, server CPU%, and errors.
- Explicitly note:
  - May submits one operation per coroutine; v1 entered the kernel per
    operation, so batching wins were not expected. v2 now queues same-shard
    submissions and enters once per event-loop round, which closed most of the
    echo gap; churn is at parity within variance. Per-op and round-flag
    submission did not reduce the low-concurrency p99 tail, so v2 stays;
  - the fast path still issues a non-blocking syscall before submitting;
  - io_uring should win on churn and idle fan-out (no `epoll_ctl`), and may lose
    on tight ping-pong because the completion hop replaces the epoll wakeup;
  - results are honest either way; they decide whether follow-ups (deferred
    submit/batching, multishot accept, `SQPOLL`) are worth doing.
- Store raw CSV under `docs/bench/` and a short analysis in
  `docs/io_uring_benchmark.md`.

## 8. Review decisions (resolved)

1. **Unsupported kernels**: resolved — no runtime fallback. Enabling the feature
   on a kernel/container where io_uring is unavailable fails at scheduler init
   with a clear error message. The error is surfaced from `EventLoop::new`.
2. **Ring sizing**: resolved — default 1024 SQ / 2048 CQ per worker, and
   `Config` gets `set_io_uring_ring_entries` / `get_io_uring_ring_entries`
   (Linux + `io_uring` only). The value is the SQ entry count; the CQ is built
   with 2× that. The builder normalises to the next power of two and rejects
   zero with an `InvalidInput` error. Like the other `Config` setters, it must
   be called before the scheduler starts.
3. **Concurrent-wait detection**: resolved — add a per-`IoData` active-request
   slot (`Arc<IoState>` holding an `rcu_cell::RcuWeak<EventData>`, shared with
   the request `EventData` so the completion can clear it). The second slow-path
   wait on the same object marks the earlier request `ConcurrentWait` and
   submits `AsyncCancel`; its completion resumes the earlier waiter with a
   `concurrent io wait` I/O error. The coroutine is never resumed before its
   own CQE, so this preserves buffer safety. Fast-path operations that never
   wait are not registered.
4. **Address reconstruction**: resolved — keep the `peer_addr()`/peek-based
   approaches described in section 4.4.
5. **Scope**: resolved — full parity in v1.

### Remaining open question

- **CI**: GitHub-hosted runners may have io_uring restricted; tests skip rather
   than fail. A dedicated self-hosted job is out of scope for v1.

## 9. Suggested commit sequence

1. `feat: add io_uring feature scaffolding`
2. `feat: add io_uring ring layer`
3. `feat: add io_uring request state and selector`
4. `feat: add io_uring tcp adapters`
5. `feat: add io_uring udp and unix-domain adapters`
6. `feat: switch linux backend to io_uring behind io_uring feature`
7. `feat: support io timeouts on io_uring`
8. `feat: support io cancellation on io_uring`
9. `feat: support WaitIo on io_uring`
10. `test: add io_uring integration tests` / docs / CI
11. `bench: compare epoll and io_uring backends`

No commit will be made until the corresponding milestone is reviewed.
