//! Per-request completion state and per-object io data.
//!
//! The io_uring backend mirrors the Windows IOCP backend: every slow-path wait
//! owns an `EventData` (the ring's `user_data` points at it) rather than the
//! epoll backend's per-fd readiness state. `IoData` is the thin object handed
//! out with each socket; it only stores the fd, the ring shard, and the
//! active-request slot used to detect two coroutines waiting on one object.
//!
//! During bring-up these types live in a submodule so they do not clash with
//! the temporary epoll bridge re-exported from `linux_uring/mod.rs`; M4
//! promotes them to `io::sys` directly.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::sync::Arc;

use rcu_cell::RcuWeak;

use crate::coroutine_impl::{is_coroutine, CoroutineImpl};
use crate::io::thread::ASSOCIATED_IO_RET;
use crate::likely::likely;
use crate::scheduler::{get_scheduler, WORKER_ID};
use crate::sync::AtomicOption;
use crate::yield_now::get_co_para;

#[cfg(feature = "io_timeout")]
use crate::timeout_list::TimeoutHandle;

/// Cancellation reasons recorded before an `AsyncCancel` completes.
pub(crate) const CANCEL_NONE: u8 = 0;
pub(crate) const CANCEL_TIMED_OUT: u8 = 1;
pub(crate) const CANCEL_USER: u8 = 2;
pub(crate) const CANCEL_CONCURRENT_WAIT: u8 = 3;
/// A `WaitIoWaker` forced the current `wait_io` poll to return.
pub(crate) const CANCEL_WAKE: u8 = 4;

#[cfg(feature = "io_timeout")]
pub(crate) type TimerList = crate::timeout_list::TimeOutList<Arc<EventData>>;
#[cfg(feature = "io_timeout")]
pub(crate) type TimerHandle = TimeoutHandle<Arc<EventData>>;

/// State shared between an `IoData` and all of its requests.
///
/// The active slot only keeps a `Weak` request: the adapter frame owns the
/// strong reference until its CQE arrives, and a request that is dropped early
/// (for example after a submission error) must not keep itself alive through
/// the slot. `RcuWeak` gives wait-free reads and lockless writes, so the
/// submission and completion threads never block each other on this slot.
pub(crate) struct IoState {
    active: RcuWeak<EventData>,
    /// Set by `WaitIoWaker` when no poll is currently registered; the next
    /// `wait_io` consumes it and reports a forced wake instead of submitting.
    wake_pending: AtomicBool,
}

impl IoState {
    pub(crate) fn new() -> IoState {
        IoState {
            active: RcuWeak::new(),
            wake_pending: AtomicBool::new(false),
        }
    }

    /// Publish `req` as the object's current wait and return the previous
    /// request, if it is still alive.
    pub(crate) fn replace_active(&self, req: &Arc<EventData>) -> Option<Arc<EventData>> {
        self.active.write_arc(req).upgrade()
    }

    /// Read the current request without replacing the slot.
    pub(crate) fn peek_active(&self) -> Option<Arc<EventData>> {
        self.active.upgrade()
    }

    /// Mark a forced wakeup for the next `wait_io`.
    pub(crate) fn set_wake_pending(&self) {
        self.wake_pending.store(true, Ordering::Release);
    }

    /// Consume a pending forced wakeup.
    pub(crate) fn take_wake_pending(&self) -> bool {
        self.wake_pending.swap(false, Ordering::AcqRel)
    }

    /// Clear the active slot if it still points at `req`.
    ///
    /// The compare-and-take is atomic, so a concurrent replacement can never
    /// be removed by the old request's completion.
    pub(crate) fn clear_active(&self, req: &EventData) {
        // SAFETY: the returned pointer is only inspected for success/failure;
        // the exchange releases the old weak reference, so it must not be
        // dereferenced or turned back into a `Weak`.
        let _ = unsafe {
            self.active.compare_exchange(
                req as *const EventData,
                None,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
        };
    }
}

/// Completion state of one submitted operation.
///
/// The kernel's `user_data` is a pointer to this value. Strong references are
/// held by the adapter frame (on the coroutine stack), the timer entry and the
/// cancel registration, so it stays alive until its CQE is processed.
#[repr(C)]
pub(crate) struct EventData {
    fd: RawFd,
    /// Ring (and timer list) that will deliver this completion.
    pub(crate) shard: usize,
    io_state: Arc<IoState>,
    /// CQE result: bytes/positive on success, `-errno` on failure.
    result: AtomicIsize,
    #[cfg(feature = "io_timeout")]
    pub(crate) timer: AtomicOption<TimerHandle>,
    co: AtomicOption<CoroutineImpl>,
    /// Set after the SQE has been handed to the ring.
    submitted: AtomicBool,
    /// Set while an `AsyncCancel` SQE has been handed to the ring for this
    /// request; keeps timeout/concurrent-wait/user-cancel paths from queuing
    /// duplicate cancels.
    cancel_sent: AtomicBool,
    /// See `CANCEL_*` constants. Only the first reason to claim the slot wins.
    cancel_reason: AtomicU8,
}

impl EventData {
    pub(crate) fn new(fd: RawFd, shard: usize, io_state: Arc<IoState>) -> Arc<EventData> {
        Arc::new(EventData {
            fd,
            shard,
            io_state,
            result: AtomicIsize::new(0),
            #[cfg(feature = "io_timeout")]
            timer: AtomicOption::none(),
            co: AtomicOption::none(),
            submitted: AtomicBool::new(false),
            cancel_sent: AtomicBool::new(false),
            cancel_reason: AtomicU8::new(CANCEL_NONE),
        })
    }

    #[inline]
    pub(crate) fn user_data(&self) -> u64 {
        self as *const EventData as u64
    }

    #[inline]
    pub(crate) fn fd(&self) -> RawFd {
        self.fd
    }

    #[inline]
    pub(crate) fn state(&self) -> &Arc<IoState> {
        &self.io_state
    }

    #[inline]
    pub(crate) fn set_result(&self, result: i32) {
        self.result.store(result as isize, Ordering::Release);
    }

    /// Completion result: bytes on success, the errno on failure.
    ///
    /// Errors normally reach the coroutine through its `co_para`, so this is
    /// only used when the wait itself succeeded.
    pub(crate) fn result(&self) -> io::Result<usize> {
        let result = self.result.load(Ordering::Acquire);
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(io::Error::from_raw_os_error((-result) as i32))
        }
    }

    #[inline]
    pub(crate) fn store_co(&self, co: CoroutineImpl) {
        self.co.store(co);
    }

    #[inline]
    pub(crate) fn take_co(&self) -> Option<CoroutineImpl> {
        self.co.take()
    }

    #[inline]
    pub(crate) fn set_submitted(&self) {
        self.submitted.store(true, Ordering::Release);
    }

    #[inline]
    pub(crate) fn is_submitted(&self) -> bool {
        self.submitted.load(Ordering::Acquire)
    }

    /// Claim the cancel slot for `reason`; returns false if another reason
    /// already won.
    pub(crate) fn mark_cancel_reason(&self, reason: u8) -> bool {
        self.cancel_reason
            .compare_exchange(CANCEL_NONE, reason, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    #[inline]
    pub(crate) fn cancel_reason(&self) -> u8 {
        self.cancel_reason.load(Ordering::Acquire)
    }

    /// Claim the single `AsyncCancel` submission for this request.
    #[inline]
    pub(crate) fn take_cancel_slot(&self) -> bool {
        !self.cancel_sent.swap(true, Ordering::AcqRel)
    }

    #[inline]
    pub(crate) fn release_cancel_slot(&self) {
        self.cancel_sent.store(false, Ordering::Release);
    }

    /// Drop the active-request registration if it still points at this request.
    #[inline]
    pub(crate) fn clear_active(&self) {
        self.io_state.clear_active(self);
    }

    /// Disarm the io timer registered for this request, if any.
    #[cfg(feature = "io_timeout")]
    #[inline]
    pub(crate) fn take_timer(&self) {
        if let Some(handle) = self.timer.take() {
            let _ = handle.remove();
        }
    }
}

impl Drop for EventData {
    fn drop(&mut self) {
        // A request that never reached completion (e.g. its adapter returned a
        // submission error) must not leave a dead weak pointer in the object's
        // active slot.
        self.io_state.clear_active(self);
    }
}

/// Per-object io data, like the Windows backend's unit `IoData`.
pub struct IoData {
    fd: RawFd,
    state: Arc<IoState>,
}

impl IoData {
    pub fn new<T: AsRawFd + ?Sized>(t: &T) -> IoData {
        IoData {
            fd: t.as_raw_fd(),
            state: Arc::new(IoState::new()),
        }
    }

    #[inline]
    pub(crate) fn fd(&self) -> RawFd {
        self.fd
    }

    #[inline]
    pub(crate) fn state(&self) -> &Arc<IoState> {
        &self.state
    }

    /// Allocate the completion state for one wait on this object.
    ///
    /// The ring shard is chosen here, not in the constructor, so creating an
    /// `IoData` never starts the scheduler. A coroutine submits to the ring of
    /// the worker currently running it; plain threads (including the
    /// proxy-coroutine path) hash the fd like the epoll backend.
    pub(crate) fn new_request(&self) -> Arc<EventData> {
        EventData::new(self.fd, shard_for_fd(self.fd), self.state.clone())
    }

    /// Kept for API compatibility with the readiness backend; there is no
    /// readiness flag to reset.
    #[inline]
    pub fn reset(&self) -> usize {
        0
    }
}

impl AsFd for IoData {
    fn as_fd(&self) -> BorrowedFd<'_> {
        unsafe { BorrowedFd::borrow_raw(self.fd) }
    }
}

impl std::fmt::Debug for IoData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IoData = {{ fd: {} }}", self.fd)
    }
}

/// Pick the ring that will own this object's requests.
///
/// Coroutines submit to the ring of the worker currently running them (so the
/// same worker that polls the ring owns the submission fast path). Plain
/// threads, including the proxy-coroutine path, hash the fd like the epoll
/// backend does.
#[inline]
fn shard_for_fd(fd: RawFd) -> usize {
    let workers = get_scheduler().workers.max(1);
    if is_coroutine() {
        let id = WORKER_ID.get();
        if id < workers {
            return id;
        }
    }
    (fd as usize) % workers
}

pub(crate) fn from_nix_error(err: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
}

/// Decode the wait result for both coroutine and plain-thread callers.
pub(crate) fn co_io_result(io: &EventData, is_coroutine: bool) -> io::Result<usize> {
    if likely(is_coroutine) {
        match get_co_para() {
            None => io.result(),
            Some(err) => Err(err),
        }
    } else {
        match ASSOCIATED_IO_RET.with(|io_ret| io_ret.take()) {
            None => io.result(),
            Some(err) => Err(*err),
        }
    }
}

// State accessed by the coroutine frame, the timer list, the cancel path and
// the selector thread. Keep the auto-trait assumptions pinned here: if any
// field stops being Send/Sync the ring's cross-thread `user_data` contract is
// broken and the build should fail.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<EventData>();
    assert_send_sync::<IoData>();
    assert_send_sync::<IoState>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn request(fd: RawFd, state: &Arc<IoState>) -> Arc<EventData> {
        EventData::new(fd, 0, state.clone())
    }

    #[test]
    fn result_round_trips_success_and_errno() {
        let state = Arc::new(IoState::new());
        let data = request(1, &state);

        data.set_result(17);
        assert_eq!(data.result().unwrap(), 17);

        data.set_result(-libc::EAGAIN);
        assert_eq!(
            data.result().unwrap_err().raw_os_error(),
            Some(libc::EAGAIN)
        );
    }

    #[test]
    fn cancel_reason_only_takes_the_first_claim() {
        let state = Arc::new(IoState::new());
        let data = request(1, &state);

        assert!(data.mark_cancel_reason(CANCEL_TIMED_OUT));
        assert!(!data.mark_cancel_reason(CANCEL_USER));
        assert_eq!(data.cancel_reason(), CANCEL_TIMED_OUT);
    }

    #[test]
    fn active_slot_tracks_and_clears_requests() {
        let state = Arc::new(IoState::new());
        let first = request(1, &state);
        let second = request(1, &state);
        let third = request(1, &state);

        assert!(state.replace_active(&first).is_none());
        let previous = state.replace_active(&second).expect("first request");
        assert!(Arc::ptr_eq(&previous, &first));

        // a stale clear (e.g. a completion for an already replaced request)
        // must not remove the newer registration
        state.clear_active(&first);
        let previous = state.replace_active(&third).expect("second request");
        assert!(Arc::ptr_eq(&previous, &second));

        state.clear_active(&third);
        assert!(state.replace_active(&third).is_none());
    }

    #[test]
    fn cancel_slot_is_single_use_until_released() {
        let state = Arc::new(IoState::new());
        let data = request(1, &state);

        assert!(data.take_cancel_slot());
        assert!(!data.take_cancel_slot());
        data.release_cancel_slot();
        assert!(data.take_cancel_slot());
    }
}
