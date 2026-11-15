//! Backend-neutral reactor loop shared by the epoll and kqueue selectors.
//!
//! The backends differ only in the wait syscall and in how a ready event is
//! decoded. Everything else -- the interrupted-wait retry, resuming the
//! coroutine of a ready fd, the io timer bookkeeping, the local task drain
//! and releasing deregistered event data -- is identical and implemented
//! here, so the two selectors cannot drift apart.

use std::io;
#[cfg(feature = "io_timeout")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{fence, AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(feature = "io_timeout")]
use super::{timeout_handler, TimerList};
use super::{EventData, IoData, SysEvent};
use crate::scheduler::Scheduler;
use crate::timeout_list::now;

use may_queue::mpsc::Queue;

/// A ready event decoded by a backend.
pub(super) enum Ready<'a> {
    /// The backend's internal wakeup event. The backend has already cleared
    /// it, the loop only has to drain the global queue.
    Wakeup,
    /// An io event on `EventData`, with the bits the backend reported.
    Io(&'a EventData, usize),
}

/// Per-shard selector state common to every backend.
pub(super) struct ShardState {
    #[cfg(feature = "io_timeout")]
    pub(super) timer_list: TimerList,
    /// Event data of deregistered fds. It is pushed by any thread, but only
    /// the event loop that owns the shard releases it, right before waiting
    /// again.
    pub(super) free_ev: Queue<Arc<EventData>>,
    /// Set while a `free_ev` wakeup is in flight. Producers wake the owning
    /// selector only off the `false -> true` transition, so a burst of
    /// deregistrations costs at most one syscall; the owner clears it right
    /// before draining the queue.
    pub(super) free_ev_armed: AtomicBool,
    /// Absolute deadline (ns) of the wait the owning event loop is about to
    /// enter, or `u64::MAX` while it is awake. A timer producer compares its
    /// own deadline against this to skip the wakeup syscall: if the current
    /// wait already ends no later than the timer, the selector's own wakeup
    /// recomputes the timer list then and finds the new head anyway.
    ///
    /// Only the owning event loop writes it, and only immediately around its
    /// wait (published before the wait, reset to `u64::MAX` right after it),
    /// so a producer can never observe the deadline of some earlier wait.
    #[cfg(feature = "io_timeout")]
    pub(super) sleep_until: AtomicU64,
}

impl ShardState {
    pub(super) fn new() -> Self {
        ShardState {
            #[cfg(feature = "io_timeout")]
            timer_list: TimerList::new(),
            free_ev: Queue::new(),
            free_ev_armed: AtomicBool::new(false),
            #[cfg(feature = "io_timeout")]
            sleep_until: AtomicU64::new(u64::MAX),
        }
    }

    /// Wake the owning selector for a timer that became the head of its
    /// interval list at `deadline` (absolute ns), unless its current wait
    /// already ends at that deadline or earlier. In the latter case the
    /// selector wakes up on its own no later than the timer and recomputes
    /// the timer list then, so the extra wakeup would be redundant.
    #[cfg(feature = "io_timeout")]
    #[inline]
    pub(super) fn wake_timer(&self, deadline: u64, wake: impl FnOnce()) {
        if deadline < self.sleep_until.load(Ordering::Acquire) {
            wake();
        }
    }

    /// Queue the event data of a deregistered fd for release on the event
    /// loop that owns this shard.
    ///
    /// A deregistration raises no readiness event on either backend, and an
    /// idle selector is blocked in its wait syscall (forever when the
    /// `io_timeout` poll is off), so without the wakeup every closed fd's
    /// `Arc<EventData>` would be retained until some unrelated event happened
    /// to arrive. `wake` wakes that event loop; coalescing per shard means
    /// only the first push of a batch pays for it.
    pub(super) fn discard(&self, io_data: &IoData, wake: impl FnOnce()) {
        self.free_ev.push((*io_data).clone());
        if !self.free_ev_armed.swap(true, Ordering::AcqRel) {
            wake();
        }
    }

    /// Run one reactor round: wait for ready fds, resume their coroutines,
    /// fire due io timers, drain local tasks and release the event data of
    /// deregistered fds.
    ///
    /// `wait` performs the backend's wait syscall. It receives what is left of
    /// the timeout, in nanoseconds, and fills `events` with up to
    /// `events.len()` entries, returning how many it wrote; an interrupted
    /// wait must surface as `io::ErrorKind::Interrupted`. `dispatch` decodes
    /// one filled entry, clearing the backend's wakeup event if that is what
    /// it got.
    #[inline]
    pub(super) fn select<W, D>(
        &self,
        scheduler: &Scheduler,
        id: usize,
        events: &mut [SysEvent],
        requested_timeout: Option<u64>,
        mut wait: W,
        mut dispatch: D,
    ) -> io::Result<Option<u64>>
    where
        W: FnMut(Option<u64>, &mut [SysEvent]) -> io::Result<usize>,
        D: for<'e> FnMut(&'e SysEvent) -> Ready<'e>,
    {
        #[cfg(feature = "io_timeout")]
        let mut remaining = requested_timeout;
        #[cfg(not(feature = "io_timeout"))]
        // `next_expire == Some(0)` from the event loop asks for a
        // non-blocking reactor poll; anything else keeps blocking forever
        let mut remaining = requested_timeout.filter(|to| *to == 0);

        #[cfg(feature = "io_timeout")]
        {
            // Publish the absolute deadline of the wait we are about to enter,
            // just before the syscall, so that a concurrent timer producer
            // never compares against the deadline of an earlier wait.
            // `u64::MAX` means no finite deadline, which makes every timer
            // producer wake us.
            let deadline = remaining.map(|to| now().saturating_add(to));
            self.sleep_until
                .store(deadline.unwrap_or(u64::MAX), Ordering::Release);
        }
        // The wait syscalls are never restarted after a signal, regardless of
        // `SA_RESTART`, so they can fail with `EINTR`. Returning that error
        // would skip the timer scheduling, the local-queue drain and the
        // `free_ev` cleanup below, delaying io timeouts and letting the free
        // list grow without bound under frequent signals. Retry instead, but
        // only with what is left of the timeout: the interrupted call already
        // consumed part of it, and restarting the full wait on every signal
        // could postpone it forever.
        let start = now();
        let n = loop {
            match wait(remaining, events) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                    remaining = remaining.map(|to| to.saturating_sub(now().saturating_sub(start)));
                }
                Err(e) => {
                    // the selector is awake again; do not leave the stale
                    // deadline of this aborted wait behind
                    #[cfg(feature = "io_timeout")]
                    self.sleep_until.store(u64::MAX, Ordering::Release);
                    return Err(e);
                }
            }
        };

        // The wait is over: the selector is awake and the published deadline
        // is stale, so new head timers must wake it instead of comparing
        // against it.
        #[cfg(feature = "io_timeout")]
        self.sleep_until.store(u64::MAX, Ordering::Release);

        // collect coroutines
        for event in &events[..n] {
            let (data, flags) = match dispatch(event) {
                Ready::Wakeup => {
                    // this is just a wakeup event, drain the global queue
                    scheduler.collect_global(id);
                    continue;
                }
                Ready::Io(data, flags) => (data, flags),
            };

            data.io_flag.fetch_or(flags, Ordering::Release);
            // the reactor half of the handshake whose other half is the fence
            // in `IoData::publish_co`; without it the waiter's flag load and
            // this take can both miss, and the edge event is already consumed
            fence(Ordering::SeqCst);

            // first check the atomic wait, this may be grabbed by the worker first
            let wait = match data.wait.take() {
                Some(wait) => wait,
                None => continue,
            };
            let co = match wait.take() {
                Some(co) => co,
                None => continue, // a timer or a cancellation took it
            };

            // it's safe to remove the timer since we are running the timer_list in the same thread
            #[cfg(feature = "io_timeout")]
            data.take_timer();

            #[cfg(feature = "work_steal")]
            scheduler.schedule_with_id(co, id);
            #[cfg(not(feature = "work_steal"))]
            crate::coroutine_impl::run_coroutine(co);
        }

        // Deal with the timer list before draining the local queue: the
        // timeout handler resumes its coroutine inline on this worker, and
        // that coroutine can schedule more work locally (e.g. by yielding),
        // which the drain below then picks up. The expiry returned here is
        // discarded; it is recomputed after the drain, see below.
        #[cfg(feature = "io_timeout")]
        let _ = self.timer_list.schedule_timer(now(), &timeout_handler);

        // run all the local tasks
        scheduler.run_queued_tasks(id);

        // Recompute after the drain: the drain is budgeted by task count, not
        // by time, so a relative expiry sampled before it can be arbitrarily
        // stale and would make the next wait outlast the timer deadline by the
        // whole drain duration. Sampling now also fires any timer that became
        // due while the local tasks were running. A coroutine resumed by one
        // of those timers runs inline and may enqueue local work after the
        // drain; `has_local_tasks` then makes the event loop poll with a zero
        // timeout, so that work still runs on the next round instead of
        // blocking.
        #[cfg(feature = "io_timeout")]
        let next_expire = {
            let start = now();
            let next = self.timer_list.schedule_timer(start, &timeout_handler);
            // The timeout handler runs coroutine code inline, so rebase the
            // remaining wait to the current time, not to the start of this
            // call, or it would outlast the deadline by the handler's run
            // time too.
            next.map(|wait| wait.saturating_sub(now().saturating_sub(start)))
        };
        #[cfg(not(feature = "io_timeout"))]
        let next_expire = None;

        // free the unused event_data
        self.drain_free_ev();

        Ok(next_expire)
    }

    /// Release the event data queued by deregistrations. Only the thread that
    /// owns this shard may call this, right before it waits again.
    #[inline]
    pub(super) fn drain_free_ev(&self) {
        // Disarm with an RMW before draining, not a store. An arm ordered
        // before this swap is read by it (the arm is a release operation),
        // which makes the push preceding the arm visible to the drain below.
        // An arm ordered after it observes `false` and wakes the selector,
        // covering any push this drain misses. Clearing after the drain could
        // instead swallow the wakeup for a push that races with it and leave
        // the entry stranded.
        self.free_ev_armed.swap(false, Ordering::AcqRel);
        while !self.free_ev.bulk_pop().is_empty() {}
    }
}
