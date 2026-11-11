//! Single-producer, single-consumer channels for threads and coroutines.
//!
//! Unlike [`super::mpsc`], the [`Sender`] is not cloneable: there is exactly
//! one producer and one consumer, and each half is `Send` but not `Sync`. The
//! channel is unbounded, so `send` never blocks. The two halves can be mixed
//! freely — a coroutine consumer yields the worker thread while waiting, a
//! native thread parks.
//!
//! Disconnect and timeout semantics follow `std::sync::mpsc`: values already
//! queued are delivered before `try_recv`/`recv` report `Disconnected`, and
//! `recv_timeout` returns `Timeout` only when no value became available by the
//! deadline.
//!
//! Apart from the single-producer restriction (`Sender` is neither `Clone`
//! nor `Sync`), the public surface mirrors [`super::mpsc`]: the same methods,
//! re-exported error types, iterator traits, `Debug` output and unwind-safety
//! bounds.
use std::cell::Cell;
use std::fmt;
use std::io::ErrorKind;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
// Re-exported so the error types are reachable through this module's path,
// matching `super::mpsc` (and `std::sync::mpsc`).
pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::Thread;
use std::time::{Duration, Instant};

use super::delay_drop::{self, DelayDrop};
use super::AtomicOption;
use crate::coroutine_impl::{co_cancel_data, is_coroutine, CoroutineImpl, EventSource};
use crate::likely::{likely, unlikely};
use crate::scheduler::get_scheduler;
use crate::timeout_list::TimeoutHandle;
use crate::yield_now::{get_co_para, yield_with};

use may_queue::spsc::Queue;

/// The coroutine handle shared between `send`/`drop_chan`, the scheduler
/// timer and the cancel machinery; exactly one of them takes the
/// `CoroutineImpl`.
type TimeoutCo = Arc<AtomicOption<CoroutineImpl>>;

/// EventSource for `recv_timeout`. It holds its own reference to the queue:
/// once the coroutine is reachable by `send()` or the timer it may resume,
/// finish and drop the channel on another worker while `subscribe` still runs.
struct RecvTimeoutSource<T> {
    queue: Arc<InnerQueue<T>>,
    timeout: Duration,
    // the wait source lives on the coroutine stack; `Drop` waits until
    // `subscribe` has finished before tearing it down
    can_drop: DelayDrop,
    // the scheduler timer, removed if the wait ends before its deadline
    timer: AtomicPtr<TimeoutHandle<TimeoutCo>>,
}

impl<T> RecvTimeoutSource<T> {
    fn new(queue: Arc<InnerQueue<T>>, timeout: Duration) -> Self {
        RecvTimeoutSource {
            queue,
            timeout,
            can_drop: DelayDrop::new(),
            timer: AtomicPtr::new(ptr::null_mut()),
        }
    }

    fn delay_drop(&self) -> delay_drop::DropGuard<'_> {
        self.can_drop.delay_drop()
    }

    fn set_timer(
        &self,
        handle: Option<TimeoutHandle<TimeoutCo>>,
    ) -> Option<TimeoutHandle<TimeoutCo>> {
        let ptr = match handle {
            None => ptr::null_mut(),
            Some(h) => h.into_ptr(),
        };
        let old = self.timer.swap(ptr, Ordering::Relaxed);
        if old.is_null() {
            None
        } else {
            Some(unsafe { TimeoutHandle::from_ptr(old) })
        }
    }

    // remove the timer if it has not fired yet
    fn remove_timer(&self) {
        if let Some(h) = self.set_timer(None) {
            if h.is_link() {
                get_scheduler().del_timer(h);
            }
        }
    }
}

impl<T> Drop for RecvTimeoutSource<T> {
    fn drop(&mut self) {
        // A concurrent wake may have resumed the coroutine while `subscribe`
        // was still running; wait for it before touching the source.
        self.can_drop.wait();
        // A cancellation may have taken the coroutine without removing the
        // registration; drop whatever is left (a no-op on the normal path).
        self.queue.timeout_wait.clear();
        self.remove_timer();
    }
}

impl<T> EventSource for RecvTimeoutSource<T> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let _g = self.delay_drop();

        let cancel = co_cancel_data(&co);
        // Shared between the scheduler timer and `send()` wakeup (see `timeout_wait`).
        let wait_co = Arc::new(AtomicOption::some(co));
        // Register for cancellation while nothing else can resume the
        // coroutine: once `timeout_wait` is stored, its local data may be
        // released. Do not touch `cancel` after that point.
        cancel.set_co(wait_co.clone());
        if cancel.is_canceled() {
            cancel.cancel();
            return;
        }
        self.queue.timeout_wait.store(wait_co.clone());

        // Re-check after registering. The store above is a SeqCst RMW that
        // pairs with `send`/`drop_chan`'s take: if the take happened first we
        // observe the queued value / the disconnect here instead of sleeping
        // until the timer fires.
        if !self.queue.queue.is_empty() || self.queue.channels.load(Ordering::Acquire) == 0 {
            if let Some(co) = wait_co.take() {
                get_scheduler().schedule(co);
            }
            return;
        }

        // Arm the timer last: a short timeout can fire at once.
        let handle = get_scheduler().add_timer(self.timeout, wait_co);
        self.set_timer(Some(handle));
    }
}

struct Park<'a, T> {
    queue: &'a InnerQueue<T>,
    can_drop: DelayDrop,
}

impl<'a, T> Park<'a, T> {
    fn new(queue: &'a InnerQueue<T>) -> Park<'a, T> {
        Park {
            queue,
            can_drop: DelayDrop::new(),
        }
    }

    fn delay_drop(&self) -> delay_drop::DropGuard<'_> {
        self.can_drop.delay_drop()
    }
}

impl<T> Drop for Park<'_, T> {
    fn drop(&mut self) {
        // wait the kernel finish
        self.can_drop.wait();
        // A cancellation may have taken the coroutine without removing the
        // registration; drop whatever is left (a no-op on the normal path).
        self.queue.wait_co.clear();
    }
}

impl<T> EventSource for Park<'_, T> {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);
        // the queue could dropped if unpark by other thread
        let _g = self.delay_drop();

        // The handle is shared with the cancel machinery, so exactly one of
        // send/drop/cancel takes the coroutine out of it.
        let wait_co = Arc::new(AtomicOption::some(co));
        let slot = &self.queue.wait_co;
        slot.store(Blocker::new_coroutine(wait_co.clone()));

        // re-check both queued data and disconnection, only clear once after
        // resume
        if !self.queue.queue.is_empty() || self.queue.channels.load(Ordering::Acquire) == 0 {
            if let Some(w) = slot.take() {
                if let Some(co) = w.into_coroutine() {
                    // Do not resume the coroutine inline here: it can return
                    // from `recv`, drop this stack-resident `Park` and wait on
                    // this very `DelayDrop` while `subscribe` is still using
                    // it. That wait yields the coroutine, and a cancelled
                    // coroutine cannot yield, so an inline resume deadlocks
                    // the worker. Let the scheduler pick it up instead; the
                    // wake semantics are the same as `send`'s unpark.
                    get_scheduler().schedule(co);
                }
            }
            return;
        }

        // Register cancellation last, once the waiter is visible. The
        // delay-drop guard keeps the coroutine's local data (including its
        // `Cancel`) alive until subscribe returns, so `cancel` stays valid
        // even if a concurrent send resumes the coroutine right now.
        cancel.set_co(wait_co);
        if cancel.is_canceled() {
            cancel.cancel();
            // the coroutine was scheduled by cancel(); drop the stale waiter
            let _ = slot.take();
        }
    }
}

// The tagged handles below rely on the low bit being free. Both the
// coroutine slot (an `Arc` allocation) and `Box<Thread>` are pointer-aligned.
const _: () = assert!(std::mem::align_of::<AtomicOption<CoroutineImpl>>() >= 2);
const _: () = assert!(std::mem::align_of::<Thread>() >= 2);

/// A registered waiter: either a coroutine or a native thread, stored as a
/// tagged pointer so `AtomicOption<Blocker>` stays lock-free.
///
/// The coroutine case points to a shared `Arc<AtomicOption<CoroutineImpl>>`
/// (the same handle is registered with the cancel machinery). `send`,
/// `drop_chan` and `Cancel::cancel` all race to `take()` the coroutine out of
/// that slot, so exactly one of them resumes it.
struct Blocker {
    handle: NonZeroUsize,
}

impl Blocker {
    #[inline]
    fn new_coroutine(wait_co: Arc<AtomicOption<CoroutineImpl>>) -> Self {
        let handle = NonZeroUsize::new(Arc::into_raw(wait_co) as usize).unwrap();
        Blocker { handle }
    }

    #[inline]
    fn new_thread(thread: Thread) -> Self {
        let mut handle = NonZeroUsize::new(Box::into_raw(Box::new(thread)) as usize).unwrap();
        handle |= 1;
        Blocker { handle }
    }

    #[inline]
    fn into_coroutine(self) -> Option<CoroutineImpl> {
        // safety: the handle was produced by `Arc::into_raw` above
        let arc = unsafe { Arc::from_raw(self.handle.get() as *const AtomicOption<CoroutineImpl>) };
        std::mem::forget(self);
        arc.take()
    }

    #[inline]
    fn into_thread(self) -> Thread {
        let thread: Box<Thread> = unsafe { Box::from_raw((self.handle.get() & !1) as *mut _) };
        std::mem::forget(self);
        *thread
    }

    #[inline]
    fn unpark(self) {
        if (self.handle.get() & 1) == 0 {
            if let Some(co) = self.into_coroutine() {
                get_scheduler().schedule(co);
            }
        } else {
            let thread = self.into_thread();
            thread.unpark();
        }
    }
}

impl Drop for Blocker {
    fn drop(&mut self) {
        if (self.handle.get() & 1) == 0 {
            // release the shared handle; the coroutine may already have been
            // taken by send/drop/cancel
            unsafe {
                drop(Arc::from_raw(
                    self.handle.get() as *const AtomicOption<CoroutineImpl>
                ))
            };
        } else {
            let _thread: Box<Thread> = unsafe { Box::from_raw((self.handle.get() & !1) as *mut _) };
        }
    }
}

/// /////////////////////////////////////////////////////////////////////////////
/// InnerQueue
/// /////////////////////////////////////////////////////////////////////////////
struct InnerQueue<T> {
    queue: Queue<T>,
    // thread/coroutine for wake up, must use differently address for each round
    wait_co: AtomicOption<Blocker>,
    /// Coroutine waiting on `recv_timeout` (timer + send share this Arc).
    timeout_wait: AtomicOption<Arc<AtomicOption<CoroutineImpl>>>,
    // The number of tx channels which are currently using this queue.
    channels: AtomicUsize,
    // if rx is dropped
    port_dropped: AtomicBool,
}

impl<T> InnerQueue<T> {
    pub fn new() -> InnerQueue<T> {
        InnerQueue {
            queue: Queue::new(),
            wait_co: AtomicOption::none(),
            timeout_wait: AtomicOption::none(),
            channels: AtomicUsize::new(1),
            port_dropped: AtomicBool::new(false),
        }
    }

    pub fn send(&self, t: T) -> Result<(), T> {
        if unlikely(self.port_dropped.load(Ordering::Acquire)) {
            return Err(t);
        }
        self.queue.push(t);
        if let Some(co) = self.wait_co.take() {
            co.unpark();
        }
        if let Some(wait_co) = self.timeout_wait.take() {
            if let Some(co) = wait_co.take() {
                get_scheduler().schedule(co);
            }
        }
        Ok(())
    }

    pub fn recv(self: &Arc<Self>) -> Result<T, TryRecvError> {
        match self.try_recv() {
            Err(TryRecvError::Empty) => {
                if is_coroutine() {
                    let park = Park::new(self);
                    yield_with(&park);
                } else {
                    // Native-thread consumer. The naive
                    // store-blocker / check-queue / park() sequence loses
                    // wakeups: the producer's push -> take can interleave so
                    // that it sees no waiter while our re-check misses the
                    // pushed item (Dekker store/load race), leaving this
                    // thread parked forever with data in the queue. Observed
                    // deterministically via may_postgres::Client::prepare on
                    // plain test threads.
                    //
                    // Hardened: (a) SeqCst fence between announcing the
                    // waiter and re-checking the queue, pairing with the
                    // producer's push -> take RMW; (b) re-register and
                    // re-check in a loop, so spurious park returns and stale
                    // park tokens are absorbed; (c) bounded park as a
                    // last-resort backstop — a residual lost wakeup costs one
                    // interval, not a hang.
                    loop {
                        let blocker = Blocker::new_thread(std::thread::current());
                        self.wait_co.store(blocker);
                        std::sync::atomic::fence(Ordering::SeqCst);
                        match self.try_recv() {
                            Err(TryRecvError::Empty) => {
                                std::thread::park_timeout(std::time::Duration::from_millis(10));
                            }
                            data => {
                                self.wait_co.clear();
                                return data;
                            }
                        }
                        match self.try_recv() {
                            Err(TryRecvError::Empty) => continue,
                            data => {
                                self.wait_co.clear();
                                return data;
                            }
                        }
                    }
                }
            }
            data => return data,
        }

        // after come back try recv again (coroutine path)
        self.try_recv()
    }

    /// Receive a value with a timeout.
    ///
    /// Returns `Ok(T)` if data arrives before `timeout`.
    /// Returns `Err(RecvTimeoutError::Timeout)` if the timeout expires.
    /// Returns `Err(RecvTimeoutError::Disconnected)` if the channel is disconnected.
    fn recv_timeout_co(self: &Arc<Self>, timeout: Duration) -> Result<T, RecvTimeoutError> {
        // Quick check: if data is already available, return immediately.
        match self.try_recv() {
            Ok(t) => return Ok(t),
            Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
            Err(TryRecvError::Empty) => {}
        }

        // Create an EventSource that:
        // 1. Stores the coroutine in `timeout_wait` (for send()/drop_chan())
        // 2. Registers a scheduler timer that fires after `timeout`
        //
        // The source removes the timer from the wheel on drop, so a wait that
        // ends before its deadline does not leave an armed timer behind.
        let source = RecvTimeoutSource::new(self.clone(), timeout);
        yield_with(&source);
        self.timeout_wait.clear();

        // We are back: either data arrived (send() took wait_co and
        // unparked us) or the timer fired (which set co_para=TimedOut).
        //
        // Data is always queued BEFORE the wakeup (in send()), so if
        // data won, it's in the queue right now.
        match self.try_recv() {
            Ok(t) => Ok(t),
            Err(TryRecvError::Empty) => {
                // Queue is empty. The timer fired. Check co_para to confirm.
                if let Some(err) = get_co_para() {
                    if err.kind() == ErrorKind::TimedOut {
                        return Err(RecvTimeoutError::Timeout);
                    }
                }
                Err(RecvTimeoutError::Disconnected)
            }
            Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
        }
    }

    #[inline]
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        match self.queue.pop() {
            Some(data) => Ok(data),
            None => {
                if likely(self.channels.load(Ordering::Acquire) > 0) {
                    Err(TryRecvError::Empty)
                } else {
                    // there is no sender any more, should re-check
                    self.queue.pop().ok_or(TryRecvError::Disconnected)
                }
            }
        }
    }

    fn drop_chan(&self) {
        self.channels.store(0, Ordering::Release);
        if let Some(co) = self.wait_co.take() {
            co.unpark();
        }
        if let Some(wait_co) = self.timeout_wait.take() {
            if let Some(co) = wait_co.take() {
                get_scheduler().schedule(co);
            }
        }
    }

    pub fn drop_port(&self) {
        self.port_dropped.store(true, Ordering::Release);
        // clear all the data
        while self.queue.pop().is_some() {}
    }
}

/// The receiving half of an SPSC channel created by [`channel`].
///
/// A `Receiver` is [`Send`] but **not** [`Sync`]: it is the single consumer
/// and cannot be shared. It can be moved between threads or used from a
/// coroutine.
///
/// ```compile_fail,E0277
/// fn assert_sync<T: Sync>() {}
/// assert_sync::<may::sync::spsc::Receiver<i32>>();
/// ```
pub struct Receiver<T> {
    inner: Arc<InnerQueue<T>>,
    // `Cell<()>` is `Send` but `!Sync`, which keeps `Receiver` `!Sync` (the
    // queue's `pop` uses unsynchronized consumer-side access). Zero-sized.
    _marker: PhantomData<Cell<()>>,
}

unsafe impl<T: Send> Send for Receiver<T> {}
// Values are owned by the queue and only moved out; matching `mpsc`.
impl<T> UnwindSafe for Receiver<T> {}
impl<T> RefUnwindSafe for Receiver<T> {}

/// An iterator over the values of a [`Receiver`], blocking until the channel
/// disconnects.
#[derive(Debug)]
pub struct Iter<'a, T: 'a> {
    rx: &'a Receiver<T>,
}

/// A non-blocking iterator over the currently buffered values of a
/// [`Receiver`].
#[derive(Debug)]
pub struct TryIter<'a, T: 'a> {
    rx: &'a Receiver<T>,
}

/// An owning iterator over the values of a [`Receiver`].
#[derive(Debug)]
pub struct IntoIter<T> {
    rx: Receiver<T>,
}

/// The sending half of an SPSC channel created by [`channel`].
///
/// A `Sender` is not cloneable and is **not** [`Sync`]: it is the single
/// producer, unlike [`super::mpsc::Sender`]. It can be moved between threads
/// or used from a coroutine.
///
/// ```compile_fail,E0277
/// fn assert_sync<T: Sync>() {}
/// assert_sync::<may::sync::spsc::Sender<i32>>();
/// ```
pub struct Sender<T> {
    inner: Arc<InnerQueue<T>>,
    // `Cell<()>` is `Send` but `!Sync`, which keeps `Sender` `!Sync` (the
    // queue's `push` uses unsynchronized producer-side access). Zero-sized.
    _marker: PhantomData<Cell<()>>,
}

unsafe impl<T: Send> Send for Sender<T> {}
// Values are owned by the queue and only moved out; matching `mpsc`.
impl<T> UnwindSafe for Sender<T> {}
impl<T> RefUnwindSafe for Sender<T> {}

/// Create an unbounded single-producer, single-consumer channel.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let a = Arc::new(InnerQueue::new());
    (Sender::new(a.clone()), Receiver::new(a))
}

////////////////////////////////////////////////////////////////////////////////
// Sender
////////////////////////////////////////////////////////////////////////////////

impl<T> Sender<T> {
    fn new(inner: Arc<InnerQueue<T>>) -> Sender<T> {
        Sender {
            inner,
            _marker: PhantomData,
        }
    }

    /// Send a value on the channel, returning it back on error.
    ///
    /// Returns `Err` if the receiver has been dropped. Sending never blocks:
    /// the channel is unbounded.
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        self.inner.send(t).map_err(SendError)
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.inner.drop_chan();
    }
}

impl<T> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Sender {{ .. }}")
    }
}

////////////////////////////////////////////////////////////////////////////////
// Receiver
////////////////////////////////////////////////////////////////////////////////

impl<T> Receiver<T> {
    fn new(inner: Arc<InnerQueue<T>>) -> Receiver<T> {
        Receiver {
            inner,
            _marker: PhantomData,
        }
    }

    /// Attempt to receive a value without blocking.
    ///
    /// Returns [`TryRecvError::Empty`] if no value is buffered and the sender
    /// is alive, and [`TryRecvError::Disconnected`] if the sender has been
    /// dropped and the buffer is empty.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    /// Block until a value is available or the channel is disconnected.
    ///
    /// # Panics
    ///
    /// In a coroutine context, panics if the coroutine is canceled while
    /// waiting.
    pub fn recv(&self) -> Result<T, RecvError> {
        loop {
            match self.inner.recv() {
                Err(TryRecvError::Empty) => {}
                data => return data.map_err(|_| RecvError),
            }
        }
    }

    /// Receive a value with a timeout.
    ///
    /// Returns `Ok(T)` if data arrives before `timeout`,
    /// `Err(RecvTimeoutError::Timeout)` if the deadline passes first, and
    /// `Err(RecvTimeoutError::Disconnected)` if the sender has been dropped
    /// and no value is buffered.
    ///
    /// In a coroutine the wait is driven by may's scheduler timer (and is
    /// cancellable); on a native thread it uses `std::thread::park_timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        match self.inner.try_recv() {
            Ok(t) => Ok(t),
            Err(TryRecvError::Empty) => {
                if is_coroutine() {
                    self.inner.recv_timeout_co(timeout)
                } else {
                    self.recv_timeout_thread(timeout)
                }
            }
            Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
        }
    }

    fn recv_timeout_thread(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        let start = Instant::now();
        loop {
            let blocker = Blocker::new_thread(std::thread::current());
            self.inner.wait_co.store(blocker);
            std::sync::atomic::fence(Ordering::SeqCst);
            match self.inner.try_recv() {
                Err(TryRecvError::Empty) => {}
                data => {
                    self.inner.wait_co.clear();
                    return data.map_err(|_| RecvTimeoutError::Disconnected);
                }
            }

            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                self.inner.wait_co.clear();
                return match self.inner.try_recv() {
                    Ok(t) => Ok(t),
                    Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
                    Err(TryRecvError::Empty) => Err(RecvTimeoutError::Timeout),
                };
            }

            // As in recv(), bound each park to recover from a lost wakeup.
            // An unpark token or spurious wakeup does not end the timeout.
            std::thread::park_timeout(remaining.min(Duration::from_millis(10)));
            self.inner.wait_co.clear();
        }
    }

    /// Return an iterator that blocks for each value until the channel
    /// disconnects.
    pub fn iter(&self) -> Iter<'_, T> {
        Iter { rx: self }
    }

    /// Return an iterator that never blocks, stopping as soon as the channel
    /// is empty (or disconnected).
    pub fn try_iter(&self) -> TryIter<'_, T> {
        TryIter { rx: self }
    }
}

impl<T> Iterator for Iter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.rx.recv().ok()
    }
}

impl<T> Iterator for TryIter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

impl<'a, T> IntoIterator for &'a Receiver<T> {
    type Item = T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.rx.recv().ok()
    }
}

impl<T> IntoIterator for Receiver<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> IntoIter<T> {
        IntoIter { rx: self }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.inner.drop_port();
    }
}

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Receiver {{ .. }}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::thread;
    use std::time::{Duration, Instant};

    pub fn stress_factor() -> usize {
        match env::var("RUST_TEST_STRESS") {
            Ok(val) => val.parse().unwrap(),
            Err(..) => 1,
        }
    }

    /// Compile-time checks for the parts of the public surface that mirror
    /// `super::mpsc`.
    #[test]
    fn std_api_surface() {
        // the error types are reachable through this module's path
        use crate::sync::spsc::{
            RecvError, RecvTimeoutError, SendError, TryRecvError, TrySendError,
        };
        let _: Option<RecvError> = None;
        let _: Option<RecvTimeoutError> = None;
        let _: Option<SendError<()>> = None;
        let _: Option<TryRecvError> = None;
        let _: Option<TrySendError<()>> = None;

        // all three iterators implement Debug in mpsc/std
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_debug::<Iter<'static, i32>>();
        assert_debug::<TryIter<'static, i32>>();
        assert_debug::<IntoIter<i32>>();

        // both halves are unwind-safe regardless of T, like mpsc
        fn assert_unwind<T: std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
        assert_unwind::<Sender<*mut ()>>();
        assert_unwind::<Receiver<*mut ()>>();
    }

    // -----------------------------------------------------------------------
    // recv_timeout tests — coroutine path (run inside go! block)
    // -----------------------------------------------------------------------

    #[test]
    fn recv_timeout_immediate_data() {
        let (result,) = go!(move || {
            let (tx, rx) = channel::<i32>();
            tx.send(42).unwrap();
            let result = rx.recv_timeout(Duration::from_secs(5));
            (result,)
        })
        .join()
        .unwrap();
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn recv_timeout_timer_wins() {
        let (result, elapsed) = go!(move || {
            let (_tx, rx) = channel::<i32>();
            let start = Instant::now();
            let result = rx.recv_timeout(Duration::from_millis(50));
            let elapsed = start.elapsed();
            (result, elapsed)
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(RecvTimeoutError::Timeout));
        assert!(elapsed >= Duration::from_millis(40));
        // Wakeup latency on shared CI runners can spike to hundreds of
        // milliseconds (observed on macOS), so the upper bound is only a
        // sanity check that the call returned at all.
        assert!(elapsed < Duration::from_secs(2));
    }

    #[test]
    fn recv_timeout_data_before_timeout() {
        let (result, elapsed) = go!(move || {
            let (tx, rx) = channel::<i32>();
            // Spawn a helper coroutine to send after 50ms
            go!(move || {
                thread::sleep(Duration::from_millis(50));
                tx.send(99).unwrap();
            });
            let start = Instant::now();
            let result = rx.recv_timeout(Duration::from_secs(5));
            let elapsed = start.elapsed();
            (result, elapsed)
        })
        .join()
        .unwrap();
        assert_eq!(result, Ok(99));
        // Wakeup latency on shared CI runners can spike to hundreds of
        // milliseconds (observed on macOS), so the upper bound is only a
        // sanity check that the call returned at all.
        assert!(elapsed < Duration::from_secs(2));
    }

    #[test]
    fn recv_timeout_disconnect() {
        let result = go!(move || {
            let (tx, rx) = channel::<i32>();
            drop(tx);
            rx.recv_timeout(Duration::from_secs(5))
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(RecvTimeoutError::Disconnected));
    }

    #[test]
    fn recv_timeout_short_timeout() {
        let (result, elapsed) = go!(move || {
            let (_tx, rx) = channel::<i32>();
            let start = Instant::now();
            let result = rx.recv_timeout(Duration::from_millis(5));
            let elapsed = start.elapsed();
            (result, elapsed)
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(RecvTimeoutError::Timeout));
        // Same as above: only a sanity check against a lost wakeup.
        assert!(elapsed < Duration::from_secs(1));
    }

    #[test]
    fn recv_timeout_repeated() {
        go!(move || {
            let (tx, rx) = channel::<i32>();
            // First: timeout
            assert_eq!(
                rx.recv_timeout(Duration::from_millis(10)),
                Err(RecvTimeoutError::Timeout)
            );
            // Second: data arrives
            tx.send(1).unwrap();
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(1));
            // Third: timeout again
            assert_eq!(
                rx.recv_timeout(Duration::from_millis(10)),
                Err(RecvTimeoutError::Timeout)
            );
        });
    }

    #[test]
    fn recv_timeout_multiple_senders() {
        let (result, elapsed) = go!(move || {
            let (tx, rx) = channel::<i32>();

            go!(move || {
                thread::sleep(Duration::from_millis(30));
                tx.send(7).unwrap();
            });

            let start = Instant::now();
            let result = rx.recv_timeout(Duration::from_secs(5));
            let elapsed = start.elapsed();
            (result, elapsed)
        })
        .join()
        .unwrap();
        assert_eq!(result, Ok(7));
        // Wakeup latency on shared CI runners can spike to hundreds of
        // milliseconds (observed on macOS), so the upper bound is only a
        // sanity check that the call returned at all.
        assert!(elapsed < Duration::from_secs(2));
    }

    #[test]
    fn recv_timeout_zero_duration_empty() {
        let result = go!(move || {
            let (_tx, rx) = channel::<i32>();
            rx.recv_timeout(Duration::ZERO)
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(RecvTimeoutError::Timeout));
    }

    #[test]
    fn recv_timeout_zero_duration_with_data() {
        let result = go!(move || {
            let (tx, rx) = channel::<i32>();
            tx.send(11).unwrap();
            rx.recv_timeout(Duration::ZERO)
        })
        .join()
        .unwrap();
        assert_eq!(result, Ok(11));
    }

    #[test]
    fn recv_timeout_send_wins_while_waiting() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let ready = Arc::new(AtomicBool::new(false));
        let ready2 = ready.clone();
        let (tx, rx) = channel::<i32>();
        let sender = thread::spawn(move || {
            while !ready2.load(Ordering::Acquire) {
                thread::yield_now();
            }
            tx.send(42).unwrap();
        });

        let result = go!(move || {
            ready.store(true, Ordering::Release);
            rx.recv_timeout(Duration::from_millis(500))
        })
        .join()
        .unwrap();
        sender.join().unwrap();
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn recv_timeout_drop_sender_while_waiting() {
        use crate::sleep::sleep;

        let result = go!(move || {
            let (tx, rx) = channel::<i32>();
            go!(move || {
                sleep(Duration::from_millis(20));
                drop(tx);
            });
            rx.recv_timeout(Duration::from_secs(5))
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(RecvTimeoutError::Disconnected));
    }

    #[test]
    fn recv_timeout_canceled_while_waiting() {
        use crate::sleep::sleep;

        let (_tx, rx) = channel::<i32>();
        let handle = go!(move || rx.recv_timeout(Duration::from_secs(5)));
        sleep(Duration::from_millis(50));
        handle.coroutine().cancel();
        assert!(handle.join().is_err());
    }

    /// A coroutine blocked in `recv()` must be woken by cancellation.
    #[test]
    fn recv_canceled_while_waiting() {
        use crate::sleep::sleep;

        let (tx, rx) = channel::<i32>();
        let h = go!(move || {
            let _ = rx.recv();
        });
        sleep(Duration::from_millis(50));
        h.coroutine().cancel();

        // bounded wait: without cancellation support join() would block
        let start = Instant::now();
        while !h.is_done() && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            h.is_done(),
            "cancel did not wake a coroutine blocked in recv()"
        );
        drop(tx);
        assert!(h.join().is_err());
    }

    /// Cancellation delivered before the coroutine parks must not spin in the
    /// waiter's Drop (the wait-kernel flag is only set while subscribe runs).
    #[test]
    fn recv_canceled_before_parking() {
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let (tx, rx) = channel::<i32>();
        let h = go!(move || {
            while r.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            let _ = rx.recv();
        });

        h.coroutine().cancel();
        running.store(false, Ordering::Release);

        let start = Instant::now();
        while !h.is_done() && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(h.is_done(), "cancel before parking hung");
        drop(tx);
        assert!(h.join().is_err());
    }

    /// A sender dropping concurrently with `recv()` must not leave the
    /// consumer parked: the waiter registration re-checks the disconnect
    /// state after announcing itself.
    #[test]
    fn recv_disconnect_race() {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        for _ in 0..200 {
            let (tx, rx) = channel::<i32>();
            let ready_tx = ready_tx.clone();
            let h = go!(move || {
                ready_tx.send(()).unwrap();
                rx.recv()
            });
            ready_rx.recv().unwrap();
            drop(tx);

            let start = Instant::now();
            while !h.is_done() && start.elapsed() < Duration::from_secs(2) {
                thread::sleep(Duration::from_millis(1));
            }
            assert!(h.is_done(), "recv() did not observe the disconnect");
            assert!(h.join().unwrap().is_err());
        }
    }

    #[test]
    fn recv_timeout_late_send_after_timeout() {
        use crate::sleep::sleep;

        go!(move || {
            let (tx, rx) = channel::<i32>();
            assert_eq!(
                rx.recv_timeout(Duration::from_millis(10)),
                Err(RecvTimeoutError::Timeout)
            );
            go!(move || {
                sleep(Duration::from_millis(10));
                tx.send(99).unwrap();
            });
            sleep(Duration::from_millis(30));
            assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(99));
        });
    }

    #[test]
    fn recv_timeout_alternates_with_blocking_recv() {
        go!(move || {
            let (tx, rx) = channel::<i32>();
            assert_eq!(
                rx.recv_timeout(Duration::from_millis(10)),
                Err(RecvTimeoutError::Timeout)
            );
            // The helper returns the sender so the join handle keeps it alive: otherwise the
            // final wait could see the drop and report Disconnected instead of Timeout.
            let sender = go!(move || {
                thread::sleep(Duration::from_millis(10));
                tx.send(1).unwrap();
                tx
            });
            assert_eq!(rx.recv().unwrap(), 1);
            assert_eq!(
                rx.recv_timeout(Duration::from_millis(10)),
                Err(RecvTimeoutError::Timeout)
            );
            drop(sender.join());
        });
    }

    #[test]
    fn recv_timeout_stress_timeouts_and_sends() {
        use crate::sleep::sleep;

        let iterations = 50 * stress_factor();
        go!(move || {
            let (tx, rx) = channel::<i32>();
            for i in 0..iterations {
                assert_eq!(
                    rx.recv_timeout(Duration::from_millis(1)),
                    Err(RecvTimeoutError::Timeout)
                );
                tx.send(i as i32).unwrap();
                assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(i as i32));
                if i % 10 == 0 {
                    sleep(Duration::from_millis(1));
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // recv_timeout tests — thread path
    // -----------------------------------------------------------------------

    #[test]
    fn recv_timeout_thread_immediate_data() {
        let (tx, rx) = channel::<i32>();
        thread::spawn(move || {
            tx.send(42).unwrap();
        });
        thread::sleep(Duration::from_millis(10));
        let result = rx.recv_timeout(Duration::from_secs(5));
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn recv_timeout_thread_timer_wins() {
        let (_tx, rx) = channel::<i32>();
        let start = Instant::now();
        let result = rx.recv_timeout(Duration::from_millis(50));
        let elapsed = start.elapsed();
        assert_eq!(result, Err(RecvTimeoutError::Timeout));
        assert!(elapsed >= Duration::from_millis(40));
        // Wakeup latency on shared CI runners can spike to hundreds of
        // milliseconds (observed on macOS), so the upper bound is only a
        // sanity check that the call returned at all.
        assert!(elapsed < Duration::from_secs(2));
    }

    #[test]
    fn recv_timeout_thread_data_before_timeout() {
        let (tx, rx) = channel::<i32>();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            tx.send(99).unwrap();
        });
        let start = Instant::now();
        let result = rx.recv_timeout(Duration::from_secs(5));
        let elapsed = start.elapsed();
        handle.join().ok();
        assert_eq!(result, Ok(99));
        // Wakeup latency on shared CI runners can spike to hundreds of
        // milliseconds (observed on macOS), so the upper bound is only a
        // sanity check that the call returned at all.
        assert!(elapsed < Duration::from_secs(2));
    }

    #[test]
    fn recv_timeout_thread_disconnect() {
        let (tx, rx) = channel::<i32>();
        drop(tx);
        let result = rx.recv_timeout(Duration::from_secs(5));
        assert_eq!(result, Err(RecvTimeoutError::Disconnected));
    }

    #[test]
    fn recv_timeout_thread_zero_duration_empty() {
        let (_tx, rx) = channel::<i32>();
        let start = Instant::now();
        let result = rx.recv_timeout(Duration::ZERO);
        let elapsed = start.elapsed();
        assert_eq!(result, Err(RecvTimeoutError::Timeout));
        // Same as above: only a sanity check against a lost wakeup.
        assert!(elapsed < Duration::from_secs(1));
    }

    #[test]
    fn recv_timeout_thread_zero_duration_with_data() {
        let (tx, rx) = channel::<i32>();
        tx.send(55).unwrap();
        assert_eq!(rx.recv_timeout(Duration::ZERO), Ok(55));
    }

    #[test]
    fn recv_timeout_thread_drop_sender_while_waiting() {
        let (tx, rx) = channel::<i32>();
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            drop(tx);
        });
        let result = rx.recv_timeout(Duration::from_secs(5));
        sender.join().unwrap();
        assert_eq!(result, Err(RecvTimeoutError::Disconnected));
    }

    #[test]
    fn recv_timeout_thread_ignores_existing_unpark_token() {
        thread::spawn(|| {
            let (_tx, rx) = channel::<i32>();
            thread::current().unpark();
            let start = Instant::now();
            assert_eq!(
                rx.recv_timeout(Duration::from_millis(100)),
                Err(RecvTimeoutError::Timeout)
            );
            assert!(start.elapsed() >= Duration::from_millis(100));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn recv_timeout_thread_disconnect_after_early_wakeup() {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (tx, rx) = channel::<i32>();
        let receiver = thread::spawn(move || {
            thread::current().unpark();
            ready_tx.send(()).unwrap();
            rx.recv_timeout(Duration::from_secs(5))
        });
        ready_rx.recv().unwrap();
        thread::sleep(Duration::from_millis(20));
        drop(tx);
        assert_eq!(
            receiver.join().unwrap(),
            Err(RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn recv_timeout_thread_data_after_early_wakeup() {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (tx, rx) = channel::<i32>();
        let receiver = thread::spawn(move || {
            thread::current().unpark();
            ready_tx.send(()).unwrap();
            rx.recv_timeout(Duration::from_secs(5))
        });
        ready_rx.recv().unwrap();
        thread::sleep(Duration::from_millis(20));
        tx.send(88).unwrap();
        assert_eq!(receiver.join().unwrap(), Ok(88));
    }

    #[test]
    fn recv_timeout_thread_send_wins_while_waiting() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let ready = Arc::new(AtomicBool::new(false));
        let ready2 = ready.clone();
        let (tx, rx) = channel::<i32>();
        let sender = thread::spawn(move || {
            while !ready2.load(Ordering::Acquire) {
                thread::yield_now();
            }
            tx.send(88).unwrap();
        });
        ready.store(true, Ordering::Release);
        let result = rx.recv_timeout(Duration::from_millis(500));
        sender.join().unwrap();
        assert_eq!(result, Ok(88));
    }

    #[test]
    fn recv_timeout_thread_repeated() {
        let (tx, rx) = channel::<i32>();
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(10)),
            Err(RecvTimeoutError::Timeout)
        );
        tx.send(3).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)), Ok(3));
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(10)),
            Err(RecvTimeoutError::Timeout)
        );
    }

    #[test]
    fn recv_timeout_thread_no_stale_blocker() {
        let (tx, rx) = channel::<i32>();
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(10)),
            Err(RecvTimeoutError::Timeout)
        );
        // send() must not find a registered waiter: if the timeout left a
        // stale blocker behind, send() would unpark us here and the next
        // park would return immediately instead of sleeping.
        tx.send(1).unwrap();
        let start = Instant::now();
        thread::park_timeout(Duration::from_millis(30));
        let elapsed = start.elapsed();
        // The park should timeout after ~30ms. We allow a range to account for
        // scheduler variance: at least 15ms (allowing early unpark by scheduler)
        // but not significantly less, and at most ~200ms (allowing some system
        // load). The key point is that if a stale blocker exists, the unpark
        // would return immediately (~0-2ms).
        assert!(elapsed >= Duration::from_millis(15),
                "park returned too early ({:?}), possible stale blocker", elapsed);
        assert!(elapsed < Duration::from_millis(200),
                "park took too long ({:?})", elapsed);
        assert_eq!(rx.try_recv(), Ok(1));
    }

    // -----------------------------------------------------------------------
    // Existing tests (unchanged)
    // -----------------------------------------------------------------------

    #[test]
    fn smoke() {
        let (tx, rx) = channel::<i32>();
        tx.send(1).unwrap();
        assert_eq!(rx.recv().unwrap(), 1);
    }

    #[test]
    fn drop_full() {
        let (tx, _rx) = channel::<Box<isize>>();
        tx.send(Box::new(1)).unwrap();
    }

    #[test]
    fn drop_full_shared() {
        let (tx, _rx) = channel::<Box<isize>>();
        tx.send(Box::new(1)).unwrap();
    }

    #[test]
    fn smoke_threads() {
        let (tx, rx) = channel::<i32>();
        let _t = thread::spawn(move || {
            tx.send(1).unwrap();
        });
        assert_eq!(rx.recv().unwrap(), 1);
    }

    #[test]
    fn smoke_coroutine() {
        let (tx, rx) = channel::<i32>();
        let _t = go!(move || {
            tx.send(1).unwrap();
        });
        assert_eq!(rx.recv().unwrap(), 1);
    }

    #[test]
    fn smoke_port_gone() {
        let (tx, rx) = channel::<i32>();
        drop(rx);
        assert!(tx.send(1).is_err());
    }

    #[test]
    fn port_gone_concurrent() {
        let (tx, rx) = channel::<i32>();
        let _t = thread::spawn(move || {
            rx.recv().unwrap();
        });
        while tx.send(1).is_ok() {}
    }

    #[test]
    fn port_gone_concurrent1() {
        let (tx, rx) = channel::<i32>();
        let _t = go!(move || {
            rx.recv().unwrap();
        });
        while tx.send(1).is_ok() {}
    }

    #[test]
    fn smoke_chan_gone() {
        let (tx, rx) = channel::<i32>();
        drop(tx);
        assert!(rx.recv().is_err());
    }

    #[test]
    fn chan_gone_concurrent() {
        let (tx, rx) = channel::<i32>();
        let _t = go!(move || {
            tx.send(1).unwrap();
            tx.send(1).unwrap();
        });
        while rx.recv().is_ok() {}
    }

    #[test]
    fn stress() {
        let (tx, rx) = channel::<i32>();
        let t = thread::spawn(move || {
            for _ in 0..10000 {
                tx.send(1).unwrap();
            }
        });
        for _ in 0..10000 {
            assert_eq!(rx.recv().unwrap(), 1);
        }
        t.join().ok().unwrap();
    }

    #[test]
    fn send_from_outside_runtime() {
        let (tx1, rx1) = channel::<()>();
        let (tx2, rx2) = channel::<i32>();
        let t1 = go!(move || {
            tx1.send(()).unwrap();
            for _ in 0..40 {
                assert_eq!(rx2.recv().unwrap(), 1);
            }
        });
        rx1.recv().unwrap();
        let t2 = go!(move || for _ in 0..40 {
            tx2.send(1).unwrap();
        });
        t1.join().ok().unwrap();
        t2.join().ok().unwrap();
    }

    #[test]
    fn recv_from_outside_runtime() {
        let (tx, rx) = channel::<i32>();
        let t = thread::spawn(move || {
            for _ in 0..40 {
                assert_eq!(rx.recv().unwrap(), 1);
            }
        });
        for _ in 0..40 {
            tx.send(1).unwrap();
        }
        t.join().ok().unwrap();
    }

    #[test]
    fn no_runtime() {
        let (tx1, rx1) = channel::<i32>();
        let (tx2, rx2) = channel::<i32>();
        let t1 = thread::spawn(move || {
            assert_eq!(rx1.recv().unwrap(), 1);
            tx2.send(2).unwrap();
        });
        let t2 = go!(move || {
            tx1.send(1).unwrap();
            assert_eq!(rx2.recv().unwrap(), 2);
        });
        t1.join().ok().unwrap();
        t2.join().ok().unwrap();
    }

    #[test]
    fn oneshot_single_thread_close_port_first() {
        // Simple test of closing without sending
        let (_tx, rx) = channel::<i32>();
        drop(rx);
    }

    #[test]
    fn oneshot_single_thread_close_chan_first() {
        // Simple test of closing without sending
        let (tx, _rx) = channel::<i32>();
        drop(tx);
    }

    #[test]
    fn oneshot_single_thread_send_port_close() {
        // Testing that the sender cleans up the payload if receiver is closed
        let (tx, rx) = channel::<Box<i32>>();
        drop(rx);
        assert!(tx.send(Box::new(0)).is_err());
    }

    #[test]
    fn oneshot_single_thread_recv_chan_close() {
        // Receiving on a closed chan will panic
        let res = go!(move || {
            let (tx, rx) = channel::<i32>();
            drop(tx);
            rx.recv().unwrap();
        })
        .join();
        // What is our res?
        assert!(res.is_err());
    }

    #[test]
    fn oneshot_single_thread_send_then_recv() {
        let (tx, rx) = channel::<Box<i32>>();
        tx.send(Box::new(10)).unwrap();
        assert!(*rx.recv().unwrap() == 10);
    }

    #[test]
    fn oneshot_single_thread_try_send_open() {
        let (tx, rx) = channel::<i32>();
        assert!(tx.send(10).is_ok());
        assert!(rx.recv().unwrap() == 10);
    }

    #[test]
    fn oneshot_single_thread_try_send_closed() {
        let (tx, rx) = channel::<i32>();
        drop(rx);
        assert!(tx.send(10).is_err());
    }

    #[test]
    fn oneshot_single_thread_try_recv_open() {
        let (tx, rx) = channel::<i32>();
        tx.send(10).unwrap();
        assert!(rx.recv() == Ok(10));
    }

    #[test]
    fn oneshot_single_thread_try_recv_closed() {
        let (tx, rx) = channel::<i32>();
        drop(tx);
        assert!(rx.recv().is_err());
    }

    #[test]
    fn oneshot_single_thread_peek_data() {
        let (tx, rx) = channel::<i32>();
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        tx.send(10).unwrap();
        assert_eq!(rx.try_recv(), Ok(10));
    }

    #[test]
    fn oneshot_single_thread_peek_close() {
        let (tx, rx) = channel::<i32>();
        drop(tx);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[test]
    fn oneshot_single_thread_peek_open() {
        let (_tx, rx) = channel::<i32>();
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn oneshot_multi_task_recv_then_send() {
        let (tx, rx) = channel::<Box<i32>>();
        let _t = thread::spawn(move || {
            assert!(*rx.recv().unwrap() == 10);
        });

        tx.send(Box::new(10)).unwrap();
    }

    #[test]
    fn oneshot_multi_task_recv_then_close() {
        let (tx, rx) = channel::<Box<i32>>();
        let _t = thread::spawn(move || {
            drop(tx);
        });
        let res = thread::spawn(move || {
            assert!(*rx.recv().unwrap() == 10);
        })
        .join();
        assert!(res.is_err());
    }

    #[test]
    fn oneshot_multi_thread_close_stress() {
        for _ in 0..stress_factor() {
            let (tx, rx) = channel::<i32>();
            let _t = thread::spawn(move || {
                drop(rx);
            });
            drop(tx);
        }
    }

    #[test]
    fn oneshot_multi_thread_send_close_stress() {
        for _ in 0..stress_factor() {
            let (tx, rx) = channel::<i32>();
            let _t = thread::spawn(move || {
                drop(rx);
            });
            let _ = thread::spawn(move || {
                tx.send(1).unwrap();
            })
            .join();
        }
    }

    #[test]
    fn oneshot_multi_thread_recv_close_stress() {
        for _ in 0..stress_factor() {
            let (tx, rx) = channel::<i32>();
            thread::spawn(move || {
                let res = thread::spawn(move || {
                    rx.recv().unwrap();
                })
                .join();
                assert!(res.is_err());
            });
            let _t = thread::spawn(move || {
                thread::spawn(move || {
                    drop(tx);
                });
            });
        }
    }

    #[test]
    fn oneshot_multi_thread_send_recv_stress() {
        for _ in 0..stress_factor() {
            let (tx, rx) = channel::<Box<isize>>();
            let _t = thread::spawn(move || {
                tx.send(Box::new(10)).unwrap();
            });
            assert!(*rx.recv().unwrap() == 10);
        }
    }

    #[test]
    fn stream_send_recv_stress() {
        for _ in 0..stress_factor() {
            let (tx, rx) = channel();

            send(tx, 0);
            recv(rx, 0);

            fn send(tx: Sender<Box<i32>>, i: i32) {
                if i == 10 {
                    return;
                }

                thread::spawn(move || {
                    tx.send(Box::new(i)).unwrap();
                    send(tx, i + 1);
                });
            }

            fn recv(rx: Receiver<Box<i32>>, i: i32) {
                if i == 10 {
                    return;
                }

                thread::spawn(move || {
                    assert!(*rx.recv().unwrap() == i);
                    recv(rx, i + 1);
                });
            }
        }
    }

    #[test]
    fn oneshot_single_thread_recv_timeout() {
        let (tx, rx) = channel();
        tx.send(()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_millis(1)), Ok(()));
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(1)),
            Err(RecvTimeoutError::Timeout)
        );
        tx.send(()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_millis(1)), Ok(()));
    }

    #[test]
    fn stress_recv_timeout_two_threads() {
        let (tx, rx) = channel();
        let stress = stress_factor() + 100;
        let timeout = Duration::from_millis(1);

        thread::spawn(move || {
            for i in 0..stress {
                if i % 2 == 0 {
                    thread::sleep(timeout * 2);
                }
                tx.send(1usize).unwrap();
            }
        });

        let mut recv_count = 0;
        loop {
            match rx.recv_timeout(timeout) {
                Ok(n) => {
                    assert_eq!(n, 1usize);
                    recv_count += 1;
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        assert_eq!(recv_count, stress);
    }

    #[test]
    fn recv_timeout_upgrade() {
        let (_tx, rx) = channel::<()>();
        let timeout = Duration::from_millis(1);

        let start = Instant::now();
        assert_eq!(rx.recv_timeout(timeout), Err(RecvTimeoutError::Timeout));
        assert!(Instant::now() >= start + timeout);
    }

    #[test]
    fn recv_a_lot() {
        // Regression test that we don't run out of stack in scheduler context
        let (tx, rx) = channel();
        for _ in 0..10000 {
            tx.send(()).unwrap();
        }
        for _ in 0..10000 {
            rx.recv().unwrap();
        }
    }

    #[test]
    fn test_nested_recv_iter() {
        let (tx, rx) = channel::<i32>();
        let (total_tx, total_rx) = channel::<i32>();

        let _t = thread::spawn(move || {
            let mut acc = 0;
            for x in rx.iter() {
                acc += x;
            }
            total_tx.send(acc).unwrap();
        });

        tx.send(3).unwrap();
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        drop(tx);
        assert_eq!(total_rx.recv().unwrap(), 6);
    }

    #[test]
    fn test_recv_iter_break() {
        let (tx, rx) = channel::<i32>();
        let (count_tx, count_rx) = channel();

        let _t = thread::spawn(move || {
            let mut count = 0;
            for x in rx.iter() {
                if count >= 3 {
                    break;
                } else {
                    count += x;
                }
            }
            count_tx.send(count).unwrap();
        });

        tx.send(2).unwrap();
        tx.send(2).unwrap();
        tx.send(2).unwrap();
        let _ = tx.send(2);
        drop(tx);
        assert_eq!(count_rx.recv().unwrap(), 4);
    }

    #[test]
    fn test_recv_try_iter() {
        let (request_tx, request_rx) = channel();
        let (response_tx, response_rx) = channel();

        // Request `x`s until we have `6`.
        let t = thread::spawn(move || {
            let mut count = 0;
            loop {
                for x in response_rx.try_iter() {
                    count += x;
                    if count == 6 {
                        return count;
                    }
                }
                request_tx.send(()).unwrap();
            }
        });

        for _ in request_rx.iter() {
            if response_tx.send(2).is_err() {
                break;
            }
        }

        assert_eq!(t.join().unwrap(), 6);
    }

    #[test]
    fn test_recv_into_iter_owned() {
        let mut iter = {
            let (tx, rx) = channel::<i32>();
            tx.send(1).unwrap();
            tx.send(2).unwrap();

            rx.into_iter()
        };
        assert_eq!(iter.next().unwrap(), 1);
        assert_eq!(iter.next().unwrap(), 2);
        assert!(iter.next().is_none());
    }

    #[test]
    fn test_recv_into_iter_borrowed() {
        let (tx, rx) = channel::<i32>();
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        drop(tx);
        let mut iter = (&rx).into_iter();
        assert_eq!(iter.next().unwrap(), 1);
        assert_eq!(iter.next().unwrap(), 2);
        assert!(iter.next().is_none());
    }

    #[test]
    fn try_recv_states() {
        let (tx1, rx1) = channel::<i32>();
        let (tx2, rx2) = channel::<()>();
        let (tx3, rx3) = channel::<()>();
        let _t = thread::spawn(move || {
            rx2.recv().unwrap();
            tx1.send(1).unwrap();
            tx3.send(()).unwrap();
            rx2.recv().unwrap();
            drop(tx1);
            tx3.send(()).unwrap();
        });

        assert_eq!(rx1.try_recv(), Err(TryRecvError::Empty));
        tx2.send(()).unwrap();
        rx3.recv().unwrap();
        assert_eq!(rx1.try_recv(), Ok(1));
        assert_eq!(rx1.try_recv(), Err(TryRecvError::Empty));
        tx2.send(()).unwrap();
        rx3.recv().unwrap();
        assert_eq!(rx1.try_recv(), Err(TryRecvError::Disconnected));
    }

    // This bug used to end up in a livelock inside of the Receiver destructor
    // because the internal state of the Shared packet was corrupted
    #[test]
    fn destroy_upgraded_shared_port_when_sender_still_active() {
        let (tx, rx) = channel();
        let (tx2, rx2) = channel();
        let _t = thread::spawn(move || {
            rx.recv().unwrap(); // wait on a oneshot
            drop(rx); // destroy a shared
            tx2.send(()).unwrap();
        });
        // make sure the other thread has gone to sleep
        for _ in 0..5000 {
            thread::yield_now();
        }

        tx.send(()).unwrap();

        // wait for the child thread to exit before we exit
        rx2.recv().unwrap();
    }
}
