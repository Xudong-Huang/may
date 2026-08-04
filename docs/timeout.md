# PRD: `may::sync::spsc::recv_with_timeout`

| Field | Value |
|-------|-------|
| **Status** | **Implemented** on branch `may-sleep` — pending merge to `microscaler/may` |
| **Priority** | High (blocks may-redis timeout API) |
| **Author** | may-redis team |
| **Date** | 2026-06-05 (revised) |
| **Crate** | `may` (`may::sync::spsc`) |
| **Implementation** | `src/sync/spsc.rs` — `Receiver::recv_with_timeout`, `RecvTimeoutSource` |
| **Downstream** | `may-redis` — wire after may release (see §10) |

---

## 1. Executive Summary

`may::sync::spsc::Receiver::recv()` blocks a coroutine indefinitely with no way to bound wait time. may-redis exposes `get_with_timeout`, `set_with_timeout`, and similar APIs, but they are **no-ops** today: the `timeout` parameter is ignored and callers still block forever on `rx.recv()`.

**Solution:** `Receiver::recv_with_timeout(Duration) -> Result<T, RecvError>` integrates with the may scheduler via `EventSource` + `yield_with`, registering both the spsc wait and a one-shot timer so whichever fires first wins.

**Current state:** The API is **implemented and tested** on the `may-sleep` branch. Remaining work is merge/release of `may`, then wiring in may-redis (§10).

---

## 2. Problem Statement

### 2.1 Current behavior (may-redis)

```rust
// src/client/client_timeout.rs — timeout is ignored
pub fn get_with_timeout<K, V>(
    client: &RedisClient,
    key: K,
    timeout: Duration,
) -> Result<V, RedisError>
where
    K: ToRedisArgs,
    V: FromRedisValue,
{
    let (tx, rx) = spsc::channel();
    let request = Request::new(/* GET ... */, tx);
    client.connection().send(request)?;
    let _ = timeout; // ← no-op
    let response = rx.recv().map_err(/* ... */)?; // ← blocks forever
    V::from_redis_value(&response)
}
```

The same pattern appears in `pubsub_client.rs` (`recv_message_timeout` uses a manual `try_recv` + deadline loop — cooperative but not scheduler-integrated) and in `pipeline.rs` (`execute_raw` uses unbounded `rx.recv()` per response).

### 2.2 Why manual timeout loops fail

| Approach | Problem |
|----------|---------|
| `try_recv()` + `yield_now()` + deadline | Burns scheduler turns; does not register with epoll/timer wheel; flaky under load |
| `thread::sleep` in a coroutine | Blocks an OS thread, not cooperative |
| External watchdog thread | Breaks single-threaded coroutine model; race-prone |
| Ignoring timeout (current) | API lies to callers; production hangs |

### 2.3 Impact

- **may-redis:** Timeout APIs are unusable for production (Sesame-IDAM migration expects redis-crate-compatible timeouts).
- **Any may consumer** using spsc for request/response (may_postgres pattern) has the same gap.
- **Tests:** Cannot assert bounded wait without stall fixtures and process kills.

---

## 3. Goals & Non-Goals

### 3.1 Goals

1. Add `recv_with_timeout` to `may::sync::spsc::Receiver`.
2. Integrate with the scheduler timer wheel (`Scheduler::add_timer`) via `EventSource`.
3. Preserve existing `recv()` semantics (no timeout path changes blocking recv).
4. Correct races: message vs timer vs disconnect (see §6).
5. Ship unit tests under `may::go!` (no `#[tokio::test]`).
6. Enable may-redis to map `RecvError::Timeout` → connection timeout error (§10).

### 3.2 Non-Goals

- Timeouts on `may::sync::mpsc`, `may::sync::spsc::Sender::send`, or TCP `read`/`write`.
- Sub-millisecond timer precision (scheduler uses millisecond buckets).
- `select!` over multiple receivers (future: `EventSource for Receiver` — see §11).
- Changing `recv()` signature or behavior.

---

## 4. API Design

### 4.1 New method

```rust
impl<T> Receiver<T> {
    /// Receive a value, waiting at most `timeout`.
    ///
    /// Returns `Ok(T)` if a sender delivers before the deadline.
    /// Returns `Err(RecvError::Timeout)` if the deadline passes with no message.
    /// Returns `Err(RecvError::Disconnected)` if all senders are dropped
    /// (whether or not the timeout has elapsed).
    pub fn recv_with_timeout(&self, timeout: Duration) -> Result<T, RecvError>;
}
```

### 4.2 Extended error type

`RecvError` gains one variant (backward-compatible for match exhaustiveness in downstream crates):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    Disconnected,
    Timeout, // NEW
}
```

| Result | Meaning |
|--------|---------|
| `Ok(T)` | Message received before deadline |
| `Err(RecvError::Timeout)` | Deadline elapsed, queue still empty |
| `Err(RecvError::Disconnected)` | All `Sender`s dropped |

**Note:** Disconnect while waiting returns `Disconnected`, not `Timeout`, even if the deadline has not passed.

### 4.3 Preconditions

| Precondition | Behavior |
|--------------|----------|
| Called outside a coroutine (`Scheduler::current() == None`) | Same as `recv()`: `try_recv()` or `RecvError::Disconnected` — **no timer** (consistent with non-coroutine `recv`) |
| `timeout == Duration::ZERO` | Non-blocking: `try_recv()` or immediate `Err(RecvError::Timeout)` |
| `timeout` very large | Behaves like `recv()` in practice |

---

## 5. Architecture (as implemented)

### 5.1 Components

```
Receiver::recv_with_timeout(timeout)
        │
        ├─ try_recv() → Ok(data)     [fast path]
        │
        └─ yield_with(RecvTimeoutSource { receiver, timeout })
                 │
                 ├─ RecvTimeoutSource::subscribe()
                 │     ├─ receiver.wait_co = WaitCo::new(current_co)
                 │     └─ scheduler.add_timer(deadline, timer_co)
                 │
                 ├─ Scheduler runs other coroutines / epoll / timers
                 │
                 └─ First wakeup:
                       ├─ Sender::send → unpark receiver → try_recv → Ok(T)
                       ├─ Timer fires → try_recv empty → Err(Timeout)
                       └─ Sender dropped → Err(Disconnected)
```

### 5.2 `RecvTimeoutSource` (private)

Located in `src/sync/spsc.rs`. Implements `EventSource`:

- **`subscribe`:** Installs `wait_co` on the receiver; registers one-shot timer via `add_timer`.
- **`try_select`:** `try_recv()` on the receiver.
- **`cancel`:** Clears `wait_co` (timer callback sees `wait_co.take() == None` and no-ops).
- **`on_exception`:** Clears `wait_co` on coroutine cancel/panic path.

Timer callback (closure passed to `add_timer`):

```rust
if let Some(wait_co) = receiver.wait_co.take() {
    wait_co.unpark(); // receiver coroutine re-runs try_select
}
// else: recv finished or was cancelled — timer is a no-op
```

This matches the **`may_postgres` / connection-loop** pattern: block on scheduler events, not spin.

### 5.3 Sequence: happy path (message wins)

```
App coroutine          Receiver              Sender (conn loop)       Scheduler
     │                    │                        │                      │
     │ recv_with_timeout  │                        │                      │
     │───────────────────>│ try_recv → Empty       │                      │
     │                    │ yield_with ──────────────────────────────────>│ register wait_co + timer
     │  (parked)          │                        │                      │
     │                    │                        │ send(data)           │
     │                    │ wait_co.unpark()       │                      │
     │                    │<───────────────────────│                      │
     │<───────────────────│ try_recv → Ok(data)    │                      │
     │ Ok(data)           │ cancel timer path      │                      │
```

### 5.4 Sequence: timeout path

```
App coroutine          Receiver              Scheduler (timer)
     │                    │                        │
     │ recv_with_timeout  │                        │
     │───────────────────>│ yield_with ───────────>│ add_timer(deadline)
     │  (parked)          │                        │
     │                    │         ... no send ...  │
     │                    │                        │ timer fires
     │                    │ wait_co.take(); unpark │
     │<───────────────────│ try_recv → Empty       │
     │ Err(Timeout)       │                        │
```

---

## 6. Race Conditions & Resolution

Both sender unpark and timer unpark use `wait_co.take()` — **at most one** wakeup is delivered to the parked receiver coroutine.

| Scenario | Order | Queue @ wake | Result |
|----------|-------|--------------|--------|
| Data before wait | `try_recv` in fast path | Has data | `Ok(data)` — no yield |
| Send then timer | Send unparks first | Has data | `Ok(data)` |
| Timer then send (send wins race) | Timer unparks; send already ran | Has data | `Ok(data)` — `try_select` after unpark |
| Timer, no send | Timer unparks | Empty | `Err(Timeout)` |
| Disconnect | `wait_co` cleared / unpark | Empty | `Err(Disconnected)` |
| Cancel / panic | `on_exception` clears `wait_co` | — | Propagate cancel |

**Stale timer after success:** On `Ok(data)`, `EventSource::cancel` runs; timer callback’s `wait_co.take()` returns `None` → harmless no-op.

---

## 7. Functional Requirements

| ID | Requirement | Status |
|----|-------------|--------|
| FR-1 | `recv_with_timeout(d)` returns `Ok(T)` when data arrives before `d` | ✅ Implemented |
| FR-2 | Returns `Err(RecvError::Timeout)` when `d` elapses with empty queue | ✅ Implemented |
| FR-3 | Returns `Err(RecvError::Disconnected)` when senders dropped | ✅ Implemented |
| FR-4 | `timeout == ZERO` → non-blocking | ✅ Implemented |
| FR-5 | Outside coroutine → no timer; same as `try_recv` / disconnect | ✅ Implemented |
| FR-6 | No regression in `recv()`, `try_recv()`, `send()` | ✅ Required at merge |
| FR-7 | Multiple concurrent `recv_with_timeout` on same `Receiver` — same as `recv()` (one waiter; undefined if two) | ✅ Same as existing |

---

## 8. Implementation Plan

### Phase 1 — `may` crate ✅ (branch `may-sleep`)

| Step | Deliverable | Status |
|------|-------------|--------|
| 1 | `RecvError::Timeout` | ✅ |
| 2 | `RecvTimeoutSource` + `EventSource` impl | ✅ |
| 3 | `Receiver::recv_with_timeout` | ✅ |
| 4 | Unit tests in `spsc.rs` | ✅ |

**Tests (must pass at merge):**

| Test | Asserts |
|------|---------|
| `recv_with_timeout_immediate` | Data already queued → `Ok` |
| `recv_with_timeout_timer_wins` | No sender → `Err(Timeout)` ~50ms |
| `recv_with_timeout_data_before_timeout` | Delayed send → `Ok` before 5s |
| `recv_with_timeout_disconnect` | Drop sender → `Err(Disconnected)` |
| `recv_with_timeout_zero_duration_*` | `Duration::ZERO` → immediate `Ok`/`Timeout` |
| `recv_with_timeout_send_wins_while_waiting` | Send races parked recv → `Ok` |
| `recv_with_timeout_drop_sender_while_waiting` | Drop sender during wait → `Disconnected` |
| `recv_with_timeout_canceled_while_waiting` | Cancel during wait → coroutine join error |
| `recv_with_timeout_late_send_after_timeout` | Timeout then later send → next recv `Ok` |
| `recv_with_timeout_stress_timeouts_and_sends` | 50× timeout/send cycles (× stress factor) |
| Thread-path mirrors | Same scenarios via `park_timeout` |

**Merge checklist:**

- [ ] `cargo test --all-features` on `may-sleep`
- [ ] `cargo clippy --all-features`
- [ ] PR to `microscaler/may` with changelog entry
- [ ] Tag / path dependency bump for may-redis

### Phase 2 — `may-redis` consumption (after may merge)

| File | Change |
|------|--------|
| `src/client/client_timeout.rs` | Replace `let _ = timeout; rx.recv()` with `recv_with_timeout(timeout)?`; map `RecvError::Timeout` → `Connection("timeout")`, `Disconnected` → existing channel-closed error |
| `src/client/pubsub_client.rs` | Replace `recv_message_timeout` poll loop with `recv_with_timeout` on push channel |
| `src/client/pipeline.rs` | Optional: `execute_raw_with_timeout` or document that pipeline timeout requires per-response `recv_with_timeout` |
| `src/core/error.rs` | Map timeout to `RedisError::Connection("timeout")` or add dedicated `Timeout` variant (redis-crate parity) |
| Integration tests | Stall server / slow reply → assert error within `timeout + slack` |

**Example wiring (client_timeout.rs):**

```rust
let response = rx.recv_with_timeout(timeout).map_err(|e| match e {
    RecvError::Timeout => RedisError::Connection("timeout".into()),
    RecvError::Disconnected => RedisError::Parse("response channel closed".into()),
})?;
```

---

## 9. Success Criteria

### `may` crate

- [x] `Receiver::recv_with_timeout` exists and is documented
- [x] Timer integrated via `EventSource` / `add_timer` (not manual spin)
- [x] All Phase 1 unit tests pass under `may::go!`
- [ ] Merged to `microscaler/may` main

### `may-redis` (post-merge)

- [ ] `get_with_timeout` / `set_with_timeout` return a timeout error when server does not respond in time
- [ ] No `let _ = timeout` in client code
- [ ] Integration test: bound wait verified (e.g. 100ms timeout, stall >500ms → fail fast)
- [ ] Pub/sub timeout uses `recv_with_timeout`, not `try_recv` loop

---

## 10. Downstream: may-redis Integration

### 10.1 Error mapping

| `RecvError` | `RedisError` |
|-------------|--------------|
| `Timeout` | `RedisError::Connection("timeout")` (or new `Timeout` variant — TBD at wire-up) |
| `Disconnected` | Parse / connection closed (existing strings) |

### 10.2 API surface to update

| API | Current | Target |
|-----|---------|--------|
| `get_with_timeout`, `set_with_timeout`, … | Ignores timeout | `recv_with_timeout` |
| `PubSubClient::recv_message_timeout` | `try_recv` + deadline | `recv_with_timeout` |
| `Pipeline::execute_raw` | Unbounded | Document no timeout; add timed variant if needed |
| BLPOP / blocking commands | Server-side block + client recv | Client recv must use timeout when API exposes one |

### 10.3 Testing strategy (may-redis)

1. **Unit:** Mock connection that never completes response channel → `recv_with_timeout` returns timeout.
2. **Integration:** `DEBUG sleep` hook or proxy delay (if available); else separate test Redis with `CLIENT PAUSE` / custom stall.
3. **Regression:** Full integration suite still passes with `--test-threads=1`.

### 10.4 Dependency

```toml
# may_redis/Cargo.toml — after may release
may = { version = "0.3", ... }  # must include recv_with_timeout
```

Until merge, may-redis can use a git/path dependency on `may-sleep` for development only.

---

## 11. Future Work (out of scope for this PRD)

| Item | Rationale |
|------|-----------|
| `impl EventSource for Receiver` | Enables `select2(rx, other)` without duplicating wait_co logic |
| `recv_with_timeout` on `may::sync::mpsc` | Broader API; not required for may-redis spsc response path |
| TCP-level read timeouts | Connection layer (`WaitIo` + socket timeouts) — orthogonal |
| Sub-ms timers | Scheduler limitation; document ms granularity |

---

## 12. Resolved Questions

| # | Question | Decision |
|---|----------|----------|
| Q1 | New error type vs extend `RecvError`? | **Extend `RecvError::Timeout`** — minimal surface, matches implementation |
| Q2 | Behavior outside coroutine? | **No timer** — `try_recv` or `Disconnected`, same spirit as `recv()` |
| Q3 | Timer granularity? | **Milliseconds** via existing `add_timer`; document imprecision |
| Q4 | Zero timeout? | **Immediate `try_recv` or `Timeout`** — implemented |
| Q5 | Pipeline timeouts? | **Phase 2 optional API**; commands already sent cannot be unsent — timeout applies per response recv |

---

## 13. Anti-Patterns (do not use)

```rust
// ❌ Ignoring timeout
let _ = timeout;
rx.recv()?;

// ❌ Spin until deadline
while start.elapsed() < timeout {
    if let Ok(v) = rx.try_recv() { return Ok(v); }
    yield_now();
}

// ❌ OS sleep in coroutine for timeout
thread::sleep(timeout);
```

```rust
// ✅ Correct
rx.recv_with_timeout(timeout)?
```

---

## Appendix A: References

| Resource | Location |
|----------|----------|
| Implementation | `may/src/sync/spsc.rs` — `recv_with_timeout`, `RecvTimeoutSource` |
| Scheduler timers | `may/src/scheduler.rs` — `add_timer` |
| EventSource | `may/src/event.rs` — `yield_with`, `select2` |
| may-redis timeout stub | `may_redis/src/client/client_timeout.rs` |
| may-redis pubsub poll loop | `may_redis/src/client/pubsub_client.rs` |
| Connection pattern | `may_postgres` — spsc response dispatch |
| may-redis wiki | `llmwiki/topics/may-coroutine-pattern.md` — prefer `recv()` over `try_recv` loops |

## Appendix B: `RecvError` (post-change)

```rust
pub enum RecvError {
    /// All senders dropped; no further messages will arrive.
    Disconnected,
    /// `recv_with_timeout` deadline elapsed with an empty queue.
    Timeout,
}
```

## Appendix C: Revision History

| Date | Change |
|------|--------|
| 2026-06-05 | Initial DRAFT |
| 2026-06-05 | Revised: aligned with `may-sleep` implementation; Phase 2 may-redis plan; resolved open questions; fixed race table and API docs |
| 2026-06-07 | Expanded test matrix: 24 `recv_with_timeout` unit tests (zero timeout, send/timer races, cancel, disconnect, stress) |
