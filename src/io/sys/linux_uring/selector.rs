//! Sharded `io_uring` selector.
//!
//! One `SingleSelector` per scheduler worker owns a ring, its io-timer list and
//! the completion-dispatch loop. `Selector::select` is called by the worker's
//! `EventLoop`; `wakeup` and `add_io_timer` may be called from any thread.
//!
//! During bring-up this module is nested under `linux_uring` (the temporary
//! epoll bridge still provides the active `sys` types); M4 promotes it.

use std::io;
#[cfg(feature = "io_timeout")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
#[cfg(feature = "io_timeout")]
use std::time::Duration;
use std::time::Duration as StdDuration;

use io_uring::{opcode, squeue};
use smallvec::SmallVec;

#[cfg(feature = "io_timeout")]
use super::event::TimerList;
use super::event::{
    EventData, IoState, CANCEL_CONCURRENT_WAIT, CANCEL_NONE, CANCEL_TIMED_OUT, CANCEL_USER,
    CANCEL_WAKE,
};
use super::ring::{Ring, SysEvent, CANCEL_USER_DATA, WAKEUP_USER_DATA};
use crate::scheduler::Scheduler;
#[cfg(feature = "io_timeout")]
use crate::timeout_list::now;
use crate::yield_now::set_co_para;

struct SingleSelector {
    ring: Ring,
    #[cfg(feature = "io_timeout")]
    timer_list: TimerList,
    /// Absolute deadline (ns) of the wait the owning event loop is about to
    /// enter, or `u64::MAX` while it is awake. A timer producer compares its
    /// own deadline against this to skip a redundant wakeup.
    #[cfg(feature = "io_timeout")]
    sleep_until: AtomicU64,
}

impl SingleSelector {
    fn new() -> io::Result<SingleSelector> {
        let entries = crate::config::config().get_io_uring_ring_entries();
        Ok(SingleSelector {
            ring: Ring::new(entries)?,
            #[cfg(feature = "io_timeout")]
            timer_list: TimerList::new(),
            #[cfg(feature = "io_timeout")]
            sleep_until: AtomicU64::new(u64::MAX),
        })
    }

    /// Submit `AsyncCancel` for one request.
    ///
    /// At most one cancel is queued per request; the slot is released again if
    /// the ring refuses the submission.
    fn cancel_request(&self, data: &EventData) -> io::Result<()> {
        // A pre-submission cancel has nothing to cancel yet. The adapter
        // re-checks the reason after its submission and cancels then; this is
        // the other half of the register-then-check handshake in `submit`.
        if !data.is_submitted() {
            return Ok(());
        }
        if !data.take_cancel_slot() {
            return Ok(());
        }
        let entry = opcode::AsyncCancel::new(data.user_data())
            .build()
            .user_data(CANCEL_USER_DATA);
        match self.ring.push_submit(entry) {
            Ok(()) => Ok(()),
            Err(e) => {
                data.release_cancel_slot();
                Err(e)
            }
        }
    }

    #[cfg(feature = "io_timeout")]
    fn timeout_handler(&self, data: std::sync::Arc<EventData>) {
        // Mark the reason before attempting the cancel. If the request is not
        // submitted yet, `AsyncCancel` would find nothing; the adapter finishes
        // the cancellation after its submission, see `cancel_if_reason_pending`.
        data.mark_cancel_reason(CANCEL_TIMED_OUT);
        data.take_timer();
        std::sync::atomic::fence(Ordering::SeqCst);
        if data.is_submitted() {
            let _ = self.cancel_request(&data);
        }
    }
}

/// One shard per scheduler worker, mirroring the epoll selector.
///
/// Shared by the scheduler across worker threads; the ring's owner-only CQ
/// consumption and SQ mutex are the only mutable parts.
pub(crate) struct Selector {
    vec: SmallVec<[SingleSelector; 128]>,
}

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Selector>();
};

impl Selector {
    pub(crate) fn new(io_workers: usize) -> io::Result<Self> {
        let mut vec = SmallVec::new();
        for id in 0..io_workers {
            let single = SingleSelector::new().map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("failed to create io_uring for worker {id}: {e}"),
                )
            })?;
            vec.push(single);
        }
        Ok(Selector { vec })
    }

    /// Process one reactor round: wait for completions, resume their
    /// coroutines, fire io timers and run local tasks.
    pub(crate) fn select(
        &self,
        scheduler: &Scheduler,
        id: usize,
        events: &mut [SysEvent],
        timeout: Option<u64>,
    ) -> io::Result<Option<u64>> {
        assert!(id < self.vec.len());
        #[cfg(not(feature = "io_timeout"))]
        // Without io timers, only a zero timeout is needed to poll the reactor
        // between budgeted local-task drains.
        let timeout = timeout.filter(|to| *to == 0);

        let single = &self.vec[id];
        #[cfg(feature = "io_timeout")]
        {
            let deadline = timeout.map(|to| now().saturating_add(to));
            single
                .sleep_until
                .store(deadline.unwrap_or(u64::MAX), Ordering::Release);
        }

        let wait = single.ring.wait(timeout.map(StdDuration::from_nanos));
        // The wait is over: the selector is awake and the published deadline
        // is stale, so new head timers must wake it instead of comparing
        // against it.
        #[cfg(feature = "io_timeout")]
        single.sleep_until.store(u64::MAX, Ordering::Release);
        wait?;

        let n = single.ring.drain(events);
        for event in &events[..n] {
            match event.user_data {
                WAKEUP_USER_DATA => {
                    scheduler.collect_global(id);
                    continue;
                }
                CANCEL_USER_DATA => {
                    trace!("io_uring cancel completion: {}", event.result);
                    continue;
                }
                _ => {}
            }

            // SAFETY: non-tag `user_data` values are `Arc<EventData>` pointers
            // written by `submit`; the adapter frame, the timer and the cancel
            // registration keep the request alive until its CQE is dispatched.
            let data = unsafe { &*(event.user_data as *const EventData) };

            // The timer list is driven by this same thread; the handle is a
            // best-effort disarm, the timer may have taken its entry already.
            #[cfg(feature = "io_timeout")]
            data.take_timer();
            // The request no longer occupies the object's active slot.
            data.clear_active();

            let mut co = match data.take_co() {
                Some(co) => co,
                None => continue,
            };

            if event.result < 0 {
                let errno = -event.result;
                if errno == libc::ECANCELED {
                    match data.cancel_reason() {
                        CANCEL_TIMED_OUT => {
                            set_co_para(&mut co, io::Error::new(io::ErrorKind::TimedOut, "timeout"))
                        }
                        CANCEL_USER => {
                            // Leave `co_para` empty: `yield_back` observes the
                            // cancel bit and raises the Cancel panic.
                        }
                        CANCEL_WAKE => {
                            // The poll was ended by a `WaitIoWaker`. Give it an
                            // error so a non-`wait_io` adapter using the same
                            // object never mistakes a canceled completion for a
                            // successful zero-byte result; `wait_io` consumes
                            // this and reports the wake bit.
                            set_co_para(
                                &mut co,
                                io::Error::new(io::ErrorKind::Interrupted, "wait_io woken"),
                            )
                        }
                        CANCEL_CONCURRENT_WAIT => {
                            set_co_para(&mut co, io::Error::other("concurrent io wait"))
                        }
                        _ => set_co_para(&mut co, io::Error::from_raw_os_error(errno)),
                    }
                } else {
                    set_co_para(&mut co, io::Error::from_raw_os_error(errno));
                }
            } else {
                data.set_result(event.result);
            }

            #[cfg(feature = "work_steal")]
            scheduler.schedule_with_id(co, id);
            #[cfg(not(feature = "work_steal"))]
            crate::coroutine_impl::run_coroutine(co);
        }

        // Fire overdue timers before draining local tasks: the timeout handler
        // only submits `AsyncCancel` (the completion resumes the coroutine
        // later), but overdue work should not wait behind the drain.
        #[cfg(feature = "io_timeout")]
        let _ = single
            .timer_list
            .schedule_timer(now(), &|data| single.timeout_handler(data));

        scheduler.run_queued_tasks(id);

        // Recompute after the drain so a timer that became due meanwhile fires
        // now, and report the wait relative to the current time.
        #[cfg(feature = "io_timeout")]
        let next_expire = single
            .timer_list
            .schedule_timer(now(), &|data| single.timeout_handler(data));
        #[cfg(not(feature = "io_timeout"))]
        let next_expire = None;

        Ok(next_expire)
    }

    /// Wake the worker that owns shard `id`.
    #[inline]
    pub(crate) fn wakeup(&self, id: usize) {
        if let Some(single) = self.vec.get(id) {
            single.ring.wakeup();
        }
    }

    /// Register the wait for `req` on the object and cancel any earlier wait
    /// that is still outstanding.
    ///
    /// The earlier waiter is resumed only by its own CQE, so its buffers stay
    /// valid; the completion maps `CANCEL_CONCURRENT_WAIT` to the same
    /// `concurrent io wait` error the readiness backend reports.
    pub(crate) fn begin_request(
        &self,
        state: &std::sync::Arc<IoState>,
        req: &std::sync::Arc<EventData>,
    ) {
        let Some(previous) = state.replace_active(req) else {
            return;
        };
        previous.mark_cancel_reason(CANCEL_CONCURRENT_WAIT);
        // Pair with the fence in `submit` (set_submitted then fence then read
        // reason): either this reason is visible to the earlier adapter, or
        // the earlier adapter's submitted flag is visible here.
        std::sync::atomic::fence(Ordering::SeqCst);
        // If the earlier adapter has not submitted yet, it will observe the
        // reason after its submission and cancel then.
        if previous.is_submitted() {
            if let Some(single) = self.vec.get(previous.shard) {
                let _ = single.cancel_request(&previous);
            }
        }
    }

    /// Cancel one request from any thread (user cancellation or a timeout).
    pub(crate) fn cancel_request(&self, data: &EventData) -> io::Result<()> {
        match self.vec.get(data.shard) {
            Some(single) => single.cancel_request(data),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "io_uring request belongs to an unknown shard",
            )),
        }
    }

    /// Submit one SQE for `req` and finish any cancel that was requested while
    /// the operation was being submitted.
    ///
    /// The caller stores the coroutine in `req` first (see `submit_co!`). The
    /// `set_submitted` + fence + reason check is the adapter half of the
    /// handshake whose other half is the fence in `cancel_request` /
    /// `timeout_handler`: either the submitter observes the reason, or the
    /// canceller observes `submitted`.
    pub(crate) fn submit(
        &self,
        req: &std::sync::Arc<EventData>,
        entry: squeue::Entry,
    ) -> io::Result<()> {
        let Some(single) = self.vec.get(req.shard) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "io_uring request belongs to an unknown shard",
            ));
        };

        self.begin_request(req.state(), req);
        let entry = entry.user_data(req.user_data());
        match single.ring.push_submit(entry) {
            Ok(()) => {
                req.set_submitted();
                std::sync::atomic::fence(Ordering::SeqCst);
                if req.cancel_reason() != CANCEL_NONE {
                    let _ = single.cancel_request(req);
                }
                Ok(())
            }
            Err(e) => {
                // The kernel does not own the SQE (a failed submit poisons the
                // ring and no later enter can pick it up), so the adapter may
                // safely resume the coroutine with the error.
                req.clear_active();
                Err(e)
            }
        }
    }

    /// Register an io timer for one request. Only compiled with `io_timeout`.
    #[cfg(feature = "io_timeout")]
    pub(crate) fn add_io_timer(&self, io: &std::sync::Arc<EventData>, timeout: Duration) {
        let Some(single) = self.vec.get(io.shard) else {
            return;
        };
        let (handle, is_head, deadline) = single.timer_list.add_timer(timeout, io.clone());
        if is_head && deadline < single.sleep_until.load(Ordering::Acquire) {
            single.ring.wakeup();
        }
        // The wait is already published; a concurrent completion may already
        // have taken the timer slot, in which case this is a best-effort disarm.
        io.timer.store(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::super::event::CANCEL_NONE;
    use super::*;
    use std::sync::Arc;

    fn selector(workers: usize) -> Option<Selector> {
        match Selector::new(workers) {
            Ok(selector) => Some(selector),
            Err(e) => {
                eprintln!("skipping io_uring selector test, ring unavailable: {e}");
                None
            }
        }
    }

    #[test]
    fn selector_builds_one_shard_per_worker() {
        let Some(selector) = selector(3) else {
            return;
        };
        assert_eq!(selector.vec.len(), 3);
    }

    #[test]
    fn second_wait_marks_the_earlier_request_concurrent() {
        let Some(selector) = selector(1) else {
            return;
        };
        let state = Arc::new(IoState::new());
        let first = EventData::new(7, 0, state.clone());
        let second = EventData::new(7, 0, state.clone());

        selector.begin_request(&state, &first);
        selector.begin_request(&state, &second);

        assert_eq!(first.cancel_reason(), CANCEL_CONCURRENT_WAIT);
        assert_eq!(second.cancel_reason(), CANCEL_NONE);
    }

    #[cfg(feature = "io_timeout")]
    #[test]
    fn zero_timer_marks_the_request_timed_out() {
        let Some(selector) = selector(1) else {
            return;
        };
        let state = Arc::new(IoState::new());
        let data = EventData::new(1, 0, state);
        selector.add_io_timer(&data, Duration::ZERO);

        let single = &selector.vec[0];
        let _ = single
            .timer_list
            .schedule_timer(now(), &|data| single.timeout_handler(data));

        assert_eq!(data.cancel_reason(), CANCEL_TIMED_OUT);
    }
}
