//! Low-level `io_uring` ring wrapper.
//!
//! One `Ring` belongs to one io worker (selector shard). The owner thread drains
//! completions; any thread may submit an SQE. Submission is serialised by a
//! shard-local mutex because the crate's `SubmissionQueue` caches head/tail
//! values and must not be used concurrently. `io_uring_enter` itself is called
//! by producers (one non-blocking submit) and by the owner (submit and wait),
//! which the kernel supports.
//!
//! Cross-thread wakeup uses a non-blocking eventfd watched by
//! `IORING_OP_POLL_ADD`. The poll is submitted with the multishot variant
//! (Linux 5.13+), so it stays registered across wakeups and only needs to be
//! re-armed when the kernel rejects multishot or a one-shot poll completes.
//! Every `wakeup` writes to the eventfd; the scheduler already coalesces
//! wakeups, so no second coalescing layer is added here.

use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use io_uring::{cqueue, opcode, squeue, types, IoUring};
use nix::sys::eventfd::{EfdFlags, EventFd};
use parking_lot::Mutex;

/// `user_data` tag of the eventfd wakeup poll.
pub(crate) const WAKEUP_USER_DATA: u64 = 1;
/// `user_data` tag of an `AsyncCancel` acknowledgement CQE.
pub(crate) const CANCEL_USER_DATA: u64 = 2;

/// A decoded completion queue entry.
///
/// The event loop owns an array of these, so they are kept independent of the
/// ring (and of the crate's `cqueue::Entry` lifetime) and are `Copy + Default`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SysEvent {
    pub user_data: u64,
    pub result: i32,
    pub flags: u32,
}

impl SysEvent {
    #[inline]
    pub(crate) fn is_wakeup(&self) -> bool {
        self.user_data == WAKEUP_USER_DATA
    }
}

/// Why an `io_uring_enter` wait returned without a completion.
enum WaitResult {
    /// At least one completion is available, or the caller was woken.
    Ready,
    /// The wait was interrupted by a signal; retry with the remaining time.
    Interrupted,
    /// The timeout elapsed before a completion arrived.
    TimedOut,
}

/// One `io_uring` instance and its wakeup eventfd.
pub(crate) struct Ring {
    uring: IoUring,
    wake_fd: EventFd,
    /// Serialises SQE mutation (`submission_shared` + push + drop).
    submit_lock: Mutex<()>,
    /// Whether a wakeup `PollAdd` is currently outstanding. With the multishot
    /// poll this stays true across wakeup completions.
    wake_armed: AtomicBool,
    /// Whether the kernel accepted the multishot wakeup poll. Starts true and
    /// permanently falls back to one-shot re-arm if the arm is rejected.
    wake_multishot: AtomicBool,
    /// A fatal submission error leaves an SQE in the ring that we must never
    /// hand to the kernel; all further submissions and waits fail.
    poisoned: AtomicBool,
    entries: usize,
    cq_entries: usize,
}

impl Ring {
    /// Build a ring and arm its wakeup poll.
    ///
    /// `entries` is the submission queue size requested by the caller; it is
    /// rounded up to a power of two. The completion queue is twice as large.
    pub(crate) fn new(entries: usize) -> io::Result<Ring> {
        let entries = entries.max(1).checked_next_power_of_two().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "io_uring submission queue size is too large",
            )
        })?;
        let entries_u32 = u32::try_from(entries).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "io_uring submission queue size exceeds u32",
            )
        })?;
        let cq_entries = entries.saturating_mul(2);
        let cq_entries_u32 = u32::try_from(cq_entries).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "io_uring completion queue size exceeds u32",
            )
        })?;

        let mut builder = IoUring::builder();
        builder.dontfork();
        builder.setup_cqsize(cq_entries_u32);
        let uring = builder.build(entries_u32)?;

        // `Ring::wait` uses the extended-argument timeout form of
        // `io_uring_enter`, which the kernel advertises through EXT_ARG
        // (Linux 5.11+). Failing here gives the scheduler a clear error instead
        // of a broken event loop.
        if !uring.params().is_feature_ext_arg() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring backend requires IORING_FEAT_EXT_ARG (Linux 5.11+)",
            ));
        }

        let wake_fd = EventFd::from_flags(EfdFlags::EFD_NONBLOCK | EfdFlags::EFD_CLOEXEC)
            .map_err(|e| io::Error::from_raw_os_error(e as i32))?;

        let ring = Ring {
            uring,
            wake_fd,
            submit_lock: Mutex::new(()),
            wake_armed: AtomicBool::new(false),
            wake_multishot: AtomicBool::new(true),
            poisoned: AtomicBool::new(false),
            entries,
            cq_entries,
        };
        ring.ensure_wake_poll()?;
        Ok(ring)
    }

    /// Submission queue entries requested at construction.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn entries(&self) -> usize {
        self.entries
    }

    /// Completion queue entries requested at construction.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn cq_entries(&self) -> usize {
        self.cq_entries
    }

    /// Push one SQE into the ring without entering the kernel.
    ///
    /// The owning worker submits it (together with everything else queued) on
    /// its next wait. The entry must already carry the caller's `user_data`.
    /// On error the caller must not retry with the same buffers: the SQE may be
    /// left in the ring, which is poisoned if submission failed after the push.
    pub(crate) fn push(&self, entry: squeue::Entry) -> io::Result<()> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(self.poisoned_error());
        }

        let _guard = self.submit_lock.lock();
        // Re-check after taking the lock: a thread that poisoned the ring
        // between our first check and the lock may have left an SQE queued
        // whose buffers are about to be freed. Never submit it later.
        if self.poisoned.load(Ordering::Acquire) {
            return Err(self.poisoned_error());
        }
        // A full queue is transient: submit what is pending, then retry the
        // push. The lock is held throughout, so no other thread can fill the
        // slot in between.
        for _ in 0..4 {
            // SAFETY: `submit_lock` guarantees no other `SubmissionQueue`
            // exists. The owner never creates one while waiting, and every
            // producer holds this lock. Pushing writes raw pointers into the
            // SQE; the caller keeps the buffers alive until the CQE.
            let mut sq = unsafe { self.uring.submission_shared() };
            let pushed = unsafe { sq.push(&entry) };
            drop(sq);
            match pushed {
                Ok(()) => return Ok(()),
                Err(_) => self.submit_all_locked()?,
            }
        }

        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "io_uring submission queue is full",
        ))
    }

    /// Push one SQE and submit all queued entries immediately.
    ///
    /// Used for cross-thread submissions and for cancels that must reach the
    /// kernel now.
    pub(crate) fn push_submit(&self, entry: squeue::Entry) -> io::Result<()> {
        self.push(entry)?;
        self.submit_queued()
    }

    /// Submit every SQE currently queued in the ring.
    ///
    /// This performs one `io_uring_enter` for the whole queue; the owning
    /// worker calls it from `wait`, and cancel paths call it to force a queued
    /// target into the kernel before submitting `AsyncCancel`.
    pub(crate) fn submit_queued(&self) -> io::Result<()> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(self.poisoned_error());
        }
        let _guard = self.submit_lock.lock();
        if self.poisoned.load(Ordering::Acquire) {
            return Err(self.poisoned_error());
        }
        self.submit_all_locked()
    }

    /// Submit every SQE currently queued in the ring.
    fn submit_all_locked(&self) -> io::Result<()> {
        let mut attempts = 0usize;
        loop {
            let pending = self.pending_locked();
            if pending == 0 {
                return Ok(());
            }

            match self.uring.submit() {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.poisoned.store(true, Ordering::Release);
                    return Err(e);
                }
            }

            attempts += 1;
            if attempts >= 8 {
                let pending = self.pending_locked();
                if pending != 0 {
                    self.poisoned.store(true, Ordering::Release);
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "io_uring submit made no progress",
                    ));
                }
                return Ok(());
            }
        }
    }

    /// Number of SQEs pushed but not yet handed to the kernel.
    ///
    /// Caller must hold `submit_lock`.
    #[inline]
    fn pending_locked(&self) -> usize {
        // SAFETY: `submit_lock` guarantees no other `SubmissionQueue` exists,
        // including the owner's wait path.
        unsafe { self.uring.submission_shared().len() }
    }

    /// Submit any pending SQEs and wait for at least one completion event,
    /// at most `timeout` (or indefinitely when `None`).
    ///
    /// Returns `Ok(())` when a completion is available or the timeout elapsed;
    /// the caller then calls [`Ring::drain`]. A signal-interrupted wait is
    /// retried with the remaining timeout.
    pub(crate) fn wait(&self, timeout: Option<Duration>) -> io::Result<()> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(self.poisoned_error());
        }
        self.ensure_wake_poll()?;

        let start = Instant::now();
        let mut remaining = timeout;
        loop {
            match self.enter_wait(remaining)? {
                WaitResult::Ready | WaitResult::TimedOut => return Ok(()),
                WaitResult::Interrupted => {
                    if let Some(dur) = remaining {
                        let elapsed = start.elapsed();
                        if elapsed >= dur {
                            return Ok(());
                        }
                        remaining = Some(dur - elapsed);
                    }
                }
            }
        }
    }

    fn enter_wait(&self, timeout: Option<Duration>) -> io::Result<WaitResult> {
        let result = match timeout {
            Some(dur) => {
                let timespec = types::Timespec::from(dur);
                let args = types::SubmitArgs::new().timespec(&timespec);
                self.uring.submitter().submit_with_args(1, &args)
            }
            None => self.uring.submitter().submit_and_wait(1),
        };

        match result {
            Ok(_) => Ok(WaitResult::Ready),
            Err(e) => {
                let error = e.raw_os_error();
                if error == Some(libc::ETIME) || error == Some(libc::ETIMEDOUT) {
                    Ok(WaitResult::TimedOut)
                } else if e.kind() == io::ErrorKind::Interrupted {
                    Ok(WaitResult::Interrupted)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Copy up to `events.len()` completion entries into `events`, advancing
    /// the completion queue.
    ///
    /// Only the ring's owner thread may call this.
    pub(crate) fn drain(&self, events: &mut [SysEvent]) -> usize {
        let mut n = 0usize;
        let mut wake_read = false;
        let mut wake_poll_ended = false;
        {
            // SAFETY: the caller is the owner thread and no other
            // `CompletionQueue` exists while this one is alive.
            let mut cq = unsafe { self.uring.completion_shared() };
            // Pair each output slot with a completion: the iterators stop when
            // either side runs out, so no indexing or length check is needed.
            for (slot, cqe) in events.iter_mut().zip(&mut cq) {
                *slot = SysEvent {
                    user_data: cqe.user_data(),
                    result: cqe.result(),
                    flags: cqe.flags(),
                };
                if slot.is_wakeup() {
                    if slot.result < 0 {
                        // The kernel rejected the multishot arm (Linux < 5.13)
                        // or the poll failed. Keep any pending eventfd counter
                        // for the one-shot poll we arm next.
                        if self.wake_multishot.swap(false, Ordering::AcqRel) {
                            trace!(
                                "io_uring multishot wake poll rejected ({}), \
                                 falling back to one-shot",
                                slot.result
                            );
                        }
                        wake_poll_ended = true;
                    } else {
                        // A valid poll completion. With multishot the kernel
                        // sets MORE and keeps the request active; a one-shot
                        // completion (or a kernel that silently ignored
                        // multishot) ends the poll.
                        wake_read = true;
                        wake_poll_ended |= !cqueue::more(slot.flags);
                    }
                }
                n += 1;
            }
        }

        if wake_read {
            // Clear the eventfd counter: a level-triggered multishot poll
            // would otherwise fire again immediately. A write that races with
            // this read leaves the counter non-zero, so the still-armed (or
            // next) poll sees it instead of losing the wakeup.
            if let Err(e) = self.wake_fd.read() {
                trace!("io_uring wakeup read failed: {e:?}");
            }
        }
        if wake_poll_ended {
            // No wakeup poll is outstanding any more; the next
            // `wait`/`ensure_wake_poll` arms a fresh one.
            self.wake_armed.store(false, Ordering::Release);
        }

        n
    }

    /// Wake the owner thread out of `wait`.
    #[inline]
    pub(crate) fn wakeup(&self) {
        if let Err(e) = self.wake_fd.write(1) {
            trace!("io_uring wakeup write failed: {e:?}");
        }
    }

    /// Arm the eventfd wakeup poll if no poll is currently outstanding.
    fn ensure_wake_poll(&self) -> io::Result<()> {
        if self.wake_armed.load(Ordering::Acquire) {
            return Ok(());
        }
        if self
            .wake_armed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let poll = opcode::PollAdd::new(types::Fd(self.wake_fd.as_raw_fd()), libc::POLLIN as u32);
        let poll = if self.wake_multishot.load(Ordering::Acquire) {
            poll.multi(true)
        } else {
            poll
        };
        let entry = poll.build().user_data(WAKEUP_USER_DATA);

        match self.push_submit(entry) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.wake_armed.store(false, Ordering::Release);
                Err(e)
            }
        }
    }

    fn poisoned_error(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "io_uring ring was poisoned by a submission failure",
        )
    }

    /// Whether the persistent multishot wake poll is in use (not the one-shot
    /// fallback). Used by the wakeup tests.
    #[cfg(test)]
    pub(crate) fn wake_poll_is_multishot(&self) -> bool {
        self.wake_multishot.load(Ordering::Acquire)
    }

    /// Whether a wake poll is currently registered.
    #[cfg(test)]
    pub(crate) fn wake_poll_armed(&self) -> bool {
        self.wake_armed.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    const TEST_UD: u64 = 0xdead_beef;

    /// Ring creation can fail on kernels/containers where io_uring is disabled.
    /// Those tests report a skip instead of failing.
    fn test_ring(entries: usize) -> Option<Ring> {
        match Ring::new(entries) {
            Ok(ring) => Some(ring),
            Err(e) => {
                eprintln!("skipping io_uring test, ring unavailable: {e}");
                None
            }
        }
    }

    fn find_event(events: &[SysEvent], user_data: u64) -> Option<SysEvent> {
        events
            .iter()
            .copied()
            .find(|event| event.user_data == user_data)
    }

    #[test]
    fn ring_sizes_are_normalized() {
        let Some(ring) = test_ring(1000) else {
            return;
        };
        assert_eq!(ring.entries(), 1024);
        assert_eq!(ring.cq_entries(), 2048);
    }

    #[test]
    fn submit_nop_and_drain_completion() {
        let Some(ring) = test_ring(1024) else {
            return;
        };

        ring.push_submit(opcode::Nop::new().build().user_data(TEST_UD))
            .unwrap();
        ring.wait(Some(Duration::from_secs(2))).unwrap();

        let mut events = [SysEvent::default(); 16];
        let n = ring.drain(&mut events);
        assert!(n > 0, "no completion arrived");
        let event = find_event(&events[..n], TEST_UD).expect("Nop completion missing");
        assert_eq!(event.result, 0);
    }

    #[test]
    fn wait_times_out_without_completions() {
        let Some(ring) = test_ring(1024) else {
            return;
        };

        let start = Instant::now();
        ring.wait(Some(Duration::from_millis(50))).unwrap();
        let elapsed = start.elapsed();
        let mut events = [SysEvent::default(); 16];
        assert_eq!(ring.drain(&mut events), 0);
        assert!(elapsed >= Duration::from_millis(40), "returned too early");
        assert!(elapsed < Duration::from_secs(5), "timeout did not fire");
    }

    #[test]
    fn queued_batch_is_submitted_together() {
        let Some(ring) = test_ring(64) else {
            return;
        };

        const TOTAL: u64 = 32;
        const BASE: u64 = 2_000;
        for i in 0..TOTAL {
            // `push` leaves every SQE in the ring; one `submit_queued` call
            // enters the kernel for the whole batch.
            ring.push(opcode::Nop::new().build().user_data(BASE + i))
                .unwrap();
        }
        ring.submit_queued().unwrap();

        let mut seen = 0u64;
        let mut events = [SysEvent::default(); 32];
        while seen < TOTAL {
            ring.wait(Some(Duration::from_secs(2))).unwrap();
            let n = ring.drain(&mut events);
            for event in &events[..n] {
                if event.user_data >= BASE {
                    assert_eq!(event.result, 0);
                    seen += 1;
                }
            }
        }
        assert_eq!(seen, TOTAL);
    }

    #[test]
    fn completion_queue_overflow_is_drained() {
        // entries=4 => completion queue of 8, so 64 completions overflow it.
        let Some(ring) = test_ring(4) else {
            return;
        };

        const TOTAL: u64 = 64;
        const BASE: u64 = 1_000;
        for i in 0..TOTAL {
            ring.push_submit(opcode::Nop::new().build().user_data(BASE + i))
                .unwrap();
        }

        let mut seen = 0u64;
        let mut events = [SysEvent::default(); 64];
        while seen < TOTAL {
            ring.wait(Some(Duration::from_secs(2))).unwrap();
            let n = ring.drain(&mut events);
            for event in &events[..n] {
                if event.user_data >= BASE {
                    assert_eq!(event.result, 0);
                    seen += 1;
                }
            }
        }
        assert_eq!(seen, TOTAL);
    }

    #[test]
    fn wakeup_round_trip_and_rearm() {
        let Some(ring) = test_ring(1024) else {
            return;
        };

        for round in 0..3 {
            ring.wakeup();
            ring.wait(Some(Duration::from_secs(2))).unwrap();

            let mut events = [SysEvent::default(); 16];
            let n = ring.drain(&mut events);
            assert!(
                events[..n].iter().any(SysEvent::is_wakeup),
                "round {round}: wakeup completion missing"
            );
        }
    }

    #[test]
    fn multishot_wake_poll_stays_armed() {
        let Some(ring) = test_ring(1024) else {
            return;
        };

        ring.wakeup();
        ring.wait(Some(Duration::from_secs(2))).unwrap();
        let mut events = [SysEvent::default(); 16];
        let n = ring.drain(&mut events);
        assert!(events[..n].iter().any(SysEvent::is_wakeup));

        // On kernels before 5.13 the arm is rejected and the ring switches to
        // one-shot re-arming; on newer kernels the poll must stay registered.
        if ring.wake_poll_is_multishot() {
            assert!(
                ring.wake_poll_armed(),
                "multishot wake poll did not stay armed"
            );
        }
    }

    #[test]
    fn concurrent_submitters_all_complete() {
        let Some(ring) = test_ring(1024) else {
            return;
        };
        let ring = Arc::new(ring);
        let threads = 4usize;
        let per_thread = 64u64;

        let handles: Vec<_> = (0..threads)
            .map(|thread| {
                let ring = ring.clone();
                std::thread::spawn(move || {
                    for i in 0..per_thread {
                        let ud = 1_000 + (thread as u64) * per_thread + i;
                        ring.push_submit(opcode::Nop::new().build().user_data(ud))
                            .unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let expected = threads as u64 * per_thread;
        let mut seen = 0u64;
        let mut events = [SysEvent::default(); 128];
        while seen < expected {
            ring.wait(Some(Duration::from_secs(2))).unwrap();
            let n = ring.drain(&mut events);
            for event in &events[..n] {
                if event.user_data >= 1_000 {
                    assert_eq!(event.result, 0);
                    seen += 1;
                }
            }
        }
        assert_eq!(seen, expected);
    }
}
