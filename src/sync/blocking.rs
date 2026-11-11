//! Low-level blocking primitives shared by the synchronization types.
//!
//! Two families exist:
//! * [`Blocker`]/[`FastBlocker`]: a one-shot parker for a single waiter,
//!   backed by a coroutine `Park` when running in a coroutine and by a
//!   [`ThreadPark`] (mutex + condition variable) otherwise.
//! * `SyncBlocker`: a [`Blocker`] with an additional hand-off protocol used by
//!   the synchronization primitives to survive the race between a waiter
//!   leaving (timeout/cancel) and a producer handing it a resource.

use parking_lot::{Condvar, Mutex};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::coroutine_impl::{current_cancel_data, is_coroutine};
use crate::park::{Park, ParkError};

/// Whether cancellation is currently disabled for the running coroutine.
///
/// `Condvar`'s internal re-lock runs with cancellation disabled so that a
/// cancel panics at a safe point instead of inside the re-lock, which means a
/// lock wait has to survive the cancel and keep waiting rather than panic.
#[inline]
pub(crate) fn cancel_disabled() -> bool {
    is_coroutine() && current_cancel_data().is_disabled()
}

/// Whether a cancel is pending that a successful park did not deliver.
///
/// `Cancel::cancel` has to take the parked coroutine out of the parker, and it
/// loses that race when a producer's `unpark` got there first: the coroutine
/// is woken normally and the cancel bit is left set with nothing to schedule.
/// The parker then returns `Ok`, so a lock wait has to re-check the bit itself
/// instead of handing the lock to a canceled waiter.
///
/// `is_canceled()` is false while cancellation is disabled, so this never
/// fires in a deferred region.
#[inline]
pub(crate) fn cancel_pending() -> bool {
    is_coroutine() && current_cancel_data().is_canceled()
}

/// A parker for the current OS thread, built on a mutex-protected token and a
/// condition variable.
///
/// The token makes `unpark` before `park` safe: `unpark` sets the token and
/// notifies, and `park_timeout` consumes it. Only a single waiter may use one
/// `ThreadPark` at a time: the notification is `notify_one`-style and the
/// token is cleared on every return.
#[derive(Debug)]
pub(crate) struct ThreadPark {
    lock: Mutex<usize>,
    cvar: Condvar,
}

impl ThreadPark {
    pub fn new() -> Self {
        ThreadPark {
            lock: Mutex::new(0),
            cvar: Condvar::new(),
        }
    }

    /// Block the current thread until a token is set by [`unpark`] or the
    /// timeout expires.
    ///
    /// Returns `Ok(())` if a token was available (possibly set before this
    /// call) and `Err(ParkError::Timeout)` if the deadline passed.
    ///
    /// The token is always cleared before returning. In particular, an
    /// `unpark` that races with the timeout may be reported as `Err` and its
    /// token discarded, so callers that care must re-check their own state
    /// afterwards.
    ///
    /// [`unpark`]: Self::unpark
    pub fn park_timeout(&self, dur: Option<Duration>) -> Result<(), ParkError> {
        let start = Instant::now();
        let mut result = Ok(());
        let mut guard = self.lock.lock();
        while *guard == 0 && result.is_ok() {
            match dur {
                None => self.cvar.wait(&mut guard),
                Some(t) => {
                    // Recompute the remaining time on every iteration so the
                    // total wait stays bounded by `t`, even if the condition
                    // variable wakes spuriously.
                    let elapsed = start.elapsed();
                    if elapsed >= t {
                        result = Err(ParkError::Timeout);
                    } else {
                        let r = self.cvar.wait_for(&mut guard, t - elapsed);
                        if r.timed_out() {
                            result = Err(ParkError::Timeout);
                        }
                    }
                }
            };
        }
        // must clear the status
        *guard = 0;
        result
    }

    /// Set the token and wake the waiter, if any.
    ///
    /// The notification is only sent when no token is already pending, which
    /// keeps the token one-shot.
    pub fn unpark(&self) {
        let mut guard = self.lock.lock();
        if *guard == 0 {
            *guard = 1;
            self.cvar.notify_one();
        }
    }
}

/// Internal implementation of [`Blocker`].
#[derive(Debug)]
enum Parker {
    Coroutine(Park),
    Thread(ThreadPark),
}

/// A one-shot parker usable from both coroutines and OS threads.
///
/// The variant is chosen at construction time from the current execution
/// context, so create the blocker from the same kind of context (coroutine or
/// OS thread) that will call [`Blocker::park`]. Parking a thread-context
/// blocker from a coroutine works, but parking a coroutine-context blocker
/// from an OS thread is not supported.
#[derive(Debug)]
pub struct Blocker {
    parker: Parker,
}

impl Blocker {
    /// Create a new blocker.
    ///
    /// `ignore_cancel` only applies to coroutines: when true, parking does
    /// not deliver cancellation, so the caller has to check it explicitly.
    /// OS threads cannot be canceled, so the flag is ignored for them.
    pub fn new(ignore_cancel: bool) -> Self {
        let parker = if is_coroutine() {
            let park = Park::new();
            park.ignore_cancel(ignore_cancel);
            Parker::Coroutine(park)
        } else {
            let park = ThreadPark::new();
            Parker::Thread(park)
        };

        Blocker { parker }
    }

    /// Create a new blocker for the current execution context.
    ///
    /// This always allocates a fresh blocker; it is not a thread-local
    /// singleton.
    pub fn current() -> Arc<Self> {
        Arc::new(Self::new(false))
    }

    /// Park the current thread/coroutine until [`unpark`](Self::unpark) is
    /// called or the timeout expires.
    #[inline]
    pub fn park(&self, timeout: Option<Duration>) -> Result<(), ParkError> {
        match self.parker {
            Parker::Coroutine(ref co) => co.park_timeout(timeout),
            Parker::Thread(ref t) => t.park_timeout(timeout),
        }
    }

    /// Wake the waiter and let it resume.
    #[inline]
    pub fn unpark(&self) {
        match self.parker {
            Parker::Coroutine(ref co) => co.unpark(),
            Parker::Thread(ref t) => t.unpark(),
        }
    }
}

/// A coroutine-only blocker that resumes the parked coroutine immediately on
/// the unparking thread instead of pushing it to the scheduler's ready queue.
///
/// This is an optimization for wakers that know the coroutine can make
/// progress right away (typically I/O completion). `unpark` therefore runs
/// the resumed coroutine synchronously, so the caller must not hold locks
/// that the resumed coroutine could need.
#[derive(Debug)]
pub struct FastBlocker(Park);

impl Default for FastBlocker {
    fn default() -> Self {
        Self::new()
    }
}

impl FastBlocker {
    /// Create a new fast blocker.
    ///
    /// # Panics
    ///
    /// Panics if called outside a coroutine context.
    pub fn new() -> Self {
        if !is_coroutine() {
            panic!("only possible to block coroutine");
        }

        FastBlocker(Park::new())
    }

    /// Park the current coroutine until [`unpark`](Self::unpark) is called or
    /// the timeout expires.
    #[inline]
    pub fn park(&self, timeout: Option<Duration>) -> Result<(), ParkError> {
        self.0.park_timeout(timeout)
    }

    /// Wake the waiter, running the coroutine immediately on the current
    /// thread.
    #[inline]
    pub fn unpark(&self) {
        self.0.unpark_impl(true)
    }
}

/// A one-shot blocker with an asynchronous release hand-off.
///
/// The parker ignores cancellation; the caller checks it. When the wait ends
/// without consuming the resource (timeout or cancel), the waiter and a
/// producer that wants to hand the resource over must agree on which of them
/// forwards it, and exactly one of them must.
///
/// Both halves of that handshake run under `spin_lock`, so the decision is
/// made with mutual exclusion:
///
/// 1. `park` returns `Err`, so the waiter calls [`leave_wait`];
/// 2. `leave_wait` returns `true` if the resource arrived anyway and the
///    caller must forward it;
/// 3. a producer calls [`try_hand_off`], which returns `false` if the waiter
///    had already left and the caller must forward to the next waiter.
///
/// A lock-free version of this handshake cannot work here: it would require
/// both sides to publish their own flag before reading the other's, but the
/// producer has to read `release` before deciding whether to set `unparked`,
/// so both sides can miss each other and lose the resource.
///
/// A `SyncBlocker` is used for a single wait only; the flags are never reset.
#[derive(Debug)]
pub(crate) struct SyncBlocker {
    // set once by `unpark` and never reset: a SyncBlocker is used for a
    // single wait only, so the flag only ever needs to be checked once
    unparked: AtomicBool,
    // used to register the release action
    release: AtomicBool,
    blocker: Blocker,
    // a spin lock to protect cancellation handling and release flag updates
    spin_lock: AtomicBool,
}

impl SyncBlocker {
    /// Create a new blocker for the current execution context that ignores
    /// cancellation (the caller must handle it).
    pub fn current() -> Arc<Self> {
        let blocker = Blocker::new(true);

        Arc::new(SyncBlocker {
            unparked: AtomicBool::new(false),
            release: AtomicBool::new(false),
            blocker,
            spin_lock: AtomicBool::new(false),
        })
    }

    /// Whether [`unpark`](Self::unpark) has already been called.
    #[inline]
    pub fn is_unparked(&self) -> bool {
        self.unparked.load(Ordering::SeqCst)
    }

    /// Record that the waiter is leaving without consuming the resource.
    #[inline]
    fn set_release(&self) {
        self.release.store(true, Ordering::SeqCst);
    }

    /// Take the release flag. Returns true exactly once if it was set.
    #[inline]
    fn take_release(&self) -> bool {
        self.release.swap(false, Ordering::SeqCst)
    }

    #[inline]
    pub fn park(&self, timeout: Option<Duration>) -> Result<(), ParkError> {
        self.blocker.park(timeout)
    }

    /// Wake the waiter.
    ///
    /// `unparked` is stored *before* waking so that the woken waiter observes
    /// it; the wake itself also provides the happens-before edge.
    #[inline]
    fn unpark(&self) {
        self.unparked.store(true, Ordering::SeqCst);
        self.blocker.unpark();
    }

    /// Producer half of the hand-off: offer the resource to this waiter.
    ///
    /// Returns `true` if the waiter took it (it has been unparked), `false`
    /// if it had already left — then the caller must forward the resource to
    /// the next waiter.
    #[inline]
    pub fn try_hand_off(&self) -> bool {
        self.acquire_spin_lock();
        let taken = !self.take_release();
        if taken {
            self.unpark();
        }
        self.release_spin_lock();
        taken
    }

    /// Waiter half of the hand-off, called when the wait ended without the
    /// resource (timeout or cancel).
    ///
    /// Returns `true` if the resource arrived anyway, because a hand-off raced
    /// with the cancellation — then the caller must forward it.
    #[inline]
    pub fn leave_wait(&self) -> bool {
        self.acquire_spin_lock();
        let handed = self.is_unparked();
        if !handed {
            self.set_release();
        }
        self.release_spin_lock();
        handed
    }

    #[inline]
    fn acquire_spin_lock(&self) {
        while self
            .spin_lock
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            std::hint::spin_loop();
        }
    }

    #[inline]
    fn release_spin_lock(&self) {
        self.spin_lock.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn token_before_park_returns_immediately() {
        let park = ThreadPark::new();
        park.unpark();
        assert_eq!(park.park_timeout(Some(Duration::from_secs(10))), Ok(()));
        // the token is one-shot
        assert_eq!(
            park.park_timeout(Some(Duration::from_millis(10))),
            Err(ParkError::Timeout)
        );
    }

    #[test]
    fn park_timeout_expires() {
        let park = ThreadPark::new();
        let start = Instant::now();
        assert_eq!(
            park.park_timeout(Some(Duration::from_millis(50))),
            Err(ParkError::Timeout)
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(40),
            "returned too early: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "returned too late: {elapsed:?}"
        );
    }

    #[test]
    fn unpark_wakes_parked_thread() {
        let park = Arc::new(ThreadPark::new());
        let waiter = park.clone();
        let handle = std::thread::spawn(move || waiter.park_timeout(Some(Duration::from_secs(10))));
        std::thread::sleep(Duration::from_millis(50));
        let start = Instant::now();
        park.unpark();
        assert_eq!(handle.join().unwrap(), Ok(()));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    #[should_panic(expected = "only possible to block coroutine")]
    fn fast_blocker_default_outside_coroutine_panics() {
        let _ = FastBlocker::default();
    }
}
