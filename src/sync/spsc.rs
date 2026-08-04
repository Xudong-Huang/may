//! provide single consumer single producer channel
use std::fmt;
use std::io::ErrorKind;
use std::num::NonZeroUsize;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{RecvError as StdRecvError, SendError, TryRecvError};
use std::sync::Arc;
use std::thread::Thread;
use std::time::Duration;

use super::AtomicOption;
use crate::coroutine_impl::{
    co_cancel_data, is_coroutine, run_coroutine, CoroutineImpl, EventSource,
};
use crate::likely::{likely, unlikely};
use crate::scheduler::get_scheduler;
use crate::yield_now::{get_co_para, yield_now, yield_with};

use may_queue::spsc::Queue;

/// Error returned from `Receiver::recv_with_timeout`.
#[derive(Debug, PartialEq, Eq)]
pub enum RecvError {
    /// The channel disconnected (all senders dropped).
    Disconnected,
    /// The timeout expired before data was available.
    Timeout,
}

/// EventSource for recv_with_timeout that combines:
/// - storing the coroutine in wait_co (so send() can take it)
/// - registering a scheduler timer
struct RecvTimeoutSource<'a, T> {
    queue: &'a InnerQueue<T>,
    timeout: Duration,
}

impl<T> EventSource for RecvTimeoutSource<'_, T> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);
        // Shared between the scheduler timer and `send()` wakeup (see `timeout_wait`).
        let wait_co = Arc::new(AtomicOption::some(co));
        get_scheduler().add_timer(self.timeout, wait_co.clone());
        self.queue.timeout_wait.store(wait_co.clone());
        cancel.set_co(wait_co);
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}

struct Park<'a, T> {
    queue: &'a InnerQueue<T>,
    // a flag if kernel is entered
    wait_kernel: AtomicBool,
}

impl<'a, T> Park<'a, T> {
    fn new(queue: &'a InnerQueue<T>) -> Park<'a, T> {
        Park {
            queue,
            wait_kernel: AtomicBool::new(true),
        }
    }

    fn delay_drop(&self) -> DropGuard<'_, '_, T> {
        DropGuard(self)
    }
}

impl<T> Drop for Park<'_, T> {
    fn drop(&mut self) {
        // wait the kernel finish
        while self.wait_kernel.load(Ordering::Relaxed) {
            yield_now();
        }
    }
}

pub struct DropGuard<'a, 'b, T>(&'b Park<'a, T>);

impl<T> Drop for DropGuard<'_, '_, T> {
    fn drop(&mut self) {
        self.0.wait_kernel.store(false, Ordering::Relaxed);
    }
}

impl<T> EventSource for Park<'_, T> {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        // the queue could dropped if unpark by other thread
        let _g = self.delay_drop();
        // register the coroutine
        let wait_co = &self.queue.wait_co;
        wait_co.store(Blocker::new_coroutine(co));
        // re-check the state, only clear once after resume
        if !self.queue.queue.is_empty() {
            if let Some(co) = wait_co.take() {
                run_coroutine(co.into_coroutine());
            }
            // return;
        }
    }
}

struct Blocker {
    handle: NonZeroUsize,
}

impl Blocker {
    #[inline]
    fn new_coroutine(co: CoroutineImpl) -> Self {
        let handle = NonZeroUsize::new(co.into_raw() as usize).unwrap();
        Blocker { handle }
    }

    #[inline]
    fn new_thread(thread: Thread) -> Self {
        let mut handle = NonZeroUsize::new(Box::into_raw(Box::new(thread)) as usize).unwrap();
        handle |= 1;
        Blocker { handle }
    }

    #[inline]
    fn into_coroutine(self) -> CoroutineImpl {
        let co = unsafe { CoroutineImpl::from_raw(self.handle.get() as *mut _) };
        std::mem::forget(self);
        co
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
            let co = self.into_coroutine();
            get_scheduler().schedule(co);
        } else {
            let thread = self.into_thread();
            thread.unpark();
        }
    }
}

impl Drop for Blocker {
    fn drop(&mut self) {
        if (self.handle.get() & 1) == 0 {
            // let co = self.into_coroutine();
            unreachable!()
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
    /// Coroutine waiting on `recv_with_timeout` (timer + send share this Arc).
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
        if unlikely(self.port_dropped.load(Ordering::Relaxed)) {
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
                    let blocker = Blocker::new_thread(std::thread::current());
                    self.wait_co.store(blocker);
                    match self.try_recv() {
                        Err(TryRecvError::Empty) => {
                            // no data, wait for it
                            std::thread::park();
                        }
                        data => {
                            self.wait_co.clear();
                            return data;
                        }
                    }
                }
            }
            data => return data,
        }

        // after come back try recv again
        self.try_recv()
    }

    /// Receive a value with a timeout.
    ///
    /// Returns `Ok(T)` if data arrives before `timeout`.
    /// Returns `Err(RecvError::Timeout)` if the timeout expires.
    /// Returns `Err(RecvError::Disconnected)` if the channel is disconnected.
    fn recv_with_timeout_co(self: &Arc<Self>, timeout: Duration) -> Result<T, RecvError> {
        // Quick check: if data is already available, return immediately.
        match self.try_recv() {
            Ok(t) => return Ok(t),
            Err(TryRecvError::Disconnected) => return Err(RecvError::Disconnected),
            Err(TryRecvError::Empty) => {}
        }

        // Create an EventSource that:
        // 1. Stores the coroutine as a Blocker in wait_co (for send() to take)
        // 2. Registers a scheduler timer that fires after `timeout`
        let mut source = RecvTimeoutSource {
            queue: &**self,
            timeout,
        };
        yield_with(&mut source);
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
                        return Err(RecvError::Timeout);
                    }
                }
                Err(RecvError::Disconnected)
            }
            Err(TryRecvError::Disconnected) => Err(RecvError::Disconnected),
        }
    }

    #[inline]
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        match self.queue.pop() {
            Some(data) => Ok(data),
            None => {
                if likely(self.channels.load(Ordering::Relaxed) > 0) {
                    Err(TryRecvError::Empty)
                } else {
                    // there is no sender any more, should re-check
                    self.queue.pop().ok_or(TryRecvError::Disconnected)
                }
            }
        }
    }

    fn drop_chan(&self) {
        self.channels.store(0, Ordering::Relaxed);
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
        self.port_dropped.store(true, Ordering::Relaxed);
        // clear all the data
        while self.queue.pop().is_some() {}
    }
}

pub struct Receiver<T> {
    inner: Arc<InnerQueue<T>>,
}

unsafe impl<T: Send> Send for Receiver<T> {}
// impl<T> !Sync for Receiver<T> {}

pub struct Iter<'a, T: 'a> {
    rx: &'a Receiver<T>,
}

pub struct TryIter<'a, T: 'a> {
    rx: &'a Receiver<T>,
}

pub struct IntoIter<T> {
    rx: Receiver<T>,
}

pub struct Sender<T> {
    inner: Arc<InnerQueue<T>>,
}

unsafe impl<T: Send> Send for Sender<T> {}
// impl<T> !Sync for Sender<T> {}
impl<T: Send> UnwindSafe for Sender<T> {}
impl<T: Send> RefUnwindSafe for Sender<T> {}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let a = Arc::new(InnerQueue::new());
    (Sender::new(a.clone()), Receiver::new(a))
}

////////////////////////////////////////////////////////////////////////////////
// Sender
////////////////////////////////////////////////////////////////////////////////

impl<T> Sender<T> {
    fn new(inner: Arc<InnerQueue<T>>) -> Sender<T> {
        Sender { inner }
    }

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
        Receiver { inner }
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    pub fn recv(&self) -> Result<T, StdRecvError> {
        loop {
            match self.inner.recv() {
                Err(TryRecvError::Empty) => {}
                data => return data.map_err(|_| StdRecvError),
            }
        }
    }

    /// Receive a value with a timeout.
    ///
    /// Returns `Ok(T)` if data arrives before `timeout`.
    /// Returns `Err(RecvError::Timeout)` if the timeout expires with an empty
    /// queue.
    /// Returns `Err(RecvError::Disconnected)` if the channel is disconnected.
    ///
    /// In coroutine context, uses may's scheduler timer list for cancellation.
    /// In thread context, uses `std::thread::park_timeout`.
    pub fn recv_with_timeout(&self, timeout: Duration) -> Result<T, RecvError> {
        match self.inner.try_recv() {
            Ok(t) => Ok(t),
            Err(TryRecvError::Empty) => {
                if is_coroutine() {
                    self.inner.recv_with_timeout_co(timeout)
                } else {
                    self.recv_with_timeout_thread(timeout)
                }
            }
            Err(TryRecvError::Disconnected) => Err(RecvError::Disconnected),
        }
    }

    fn recv_with_timeout_thread(&self, timeout: Duration) -> Result<T, RecvError> {
        // Store the blocker first, then try_recv to handle race condition.
        // If data arrives between try_recv and park, send() will take the
        // blocker and unpark us.
        let blocker = Blocker::new_thread(std::thread::current());
        self.inner.wait_co.store(blocker);
        match self.inner.try_recv() {
            Err(TryRecvError::Empty) => {
                // no data, wait for it with timeout
                std::thread::park_timeout(timeout);
            }
            data => {
                self.inner.wait_co.clear();
                return data.map_err(|_| RecvError::Disconnected);
            }
        }

        // After waking (from data or timeout), try to receive again.
        // Data might have arrived while we were parked.
        match self.inner.try_recv() {
            Ok(t) => Ok(t),
            Err(TryRecvError::Disconnected) => Err(RecvError::Disconnected),
            Err(TryRecvError::Empty) => Err(RecvError::Timeout),
        }
    }

    pub fn iter(&self) -> Iter<'_, T> {
        Iter { rx: self }
    }

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

    // -----------------------------------------------------------------------
    // recv_with_timeout tests — coroutine path (run inside go! block)
    // -----------------------------------------------------------------------

    #[test]
    fn recv_with_timeout_immediate_data() {
        let (result,) = go!(move || {
            let (tx, rx) = channel::<i32>();
            tx.send(42).unwrap();
            let result = rx.recv_with_timeout(Duration::from_secs(5));
            (result,)
        })
        .join()
        .unwrap();
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn recv_with_timeout_timer_wins() {
        let (result, elapsed) = go!(move || {
            let (_tx, rx) = channel::<i32>();
            let start = Instant::now();
            let result = rx.recv_with_timeout(Duration::from_millis(50));
            let elapsed = start.elapsed();
            (result, elapsed)
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(RecvError::Timeout));
        assert!(elapsed >= Duration::from_millis(40));
        assert!(elapsed < Duration::from_millis(200));
    }

    #[test]
    fn recv_with_timeout_data_before_timeout() {
        let (result, elapsed) = go!(move || {
            let (tx, rx) = channel::<i32>();
            // Spawn a helper coroutine to send after 50ms
            go!(move || {
                thread::sleep(Duration::from_millis(50));
                tx.send(99).unwrap();
            });
            let start = Instant::now();
            let result = rx.recv_with_timeout(Duration::from_secs(5));
            let elapsed = start.elapsed();
            (result, elapsed)
        })
        .join()
        .unwrap();
        assert_eq!(result, Ok(99));
        assert!(elapsed < Duration::from_millis(200));
    }

    #[test]
    fn recv_with_timeout_disconnect() {
        let result = go!(move || {
            let (tx, rx) = channel::<i32>();
            drop(tx);
            let result = rx.recv_with_timeout(Duration::from_secs(5));
            result
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(RecvError::Disconnected));
    }

    #[test]
    fn recv_with_timeout_short_timeout() {
        let (result, elapsed) = go!(move || {
            let (_tx, rx) = channel::<i32>();
            let start = Instant::now();
            let result = rx.recv_with_timeout(Duration::from_millis(5));
            let elapsed = start.elapsed();
            (result, elapsed)
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(RecvError::Timeout));
        assert!(elapsed < Duration::from_millis(100));
    }

    #[test]
    fn recv_with_timeout_repeated() {
        go!(move || {
            let (tx, rx) = channel::<i32>();
            // First: timeout
            assert_eq!(
                rx.recv_with_timeout(Duration::from_millis(10)),
                Err(RecvError::Timeout)
            );
            // Second: data arrives
            tx.send(1).unwrap();
            assert_eq!(rx.recv_with_timeout(Duration::from_secs(5)), Ok(1));
            // Third: timeout again
            assert_eq!(
                rx.recv_with_timeout(Duration::from_millis(10)),
                Err(RecvError::Timeout)
            );
        });
    }

    #[test]
    fn recv_with_timeout_multiple_senders() {
        let (result, elapsed) = go!(move || {
            let (tx, rx) = channel::<i32>();

            go!(move || {
                thread::sleep(Duration::from_millis(30));
                tx.send(7).unwrap();
            });

            let start = Instant::now();
            let result = rx.recv_with_timeout(Duration::from_secs(5));
            let elapsed = start.elapsed();
            (result, elapsed)
        })
        .join()
        .unwrap();
        assert_eq!(result, Ok(7));
        assert!(elapsed < Duration::from_millis(200));
    }

    #[test]
    fn recv_with_timeout_zero_duration_empty() {
        let result = go!(move || {
            let (_tx, rx) = channel::<i32>();
            rx.recv_with_timeout(Duration::ZERO)
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(RecvError::Timeout));
    }

    #[test]
    fn recv_with_timeout_zero_duration_with_data() {
        let result = go!(move || {
            let (tx, rx) = channel::<i32>();
            tx.send(11).unwrap();
            rx.recv_with_timeout(Duration::ZERO)
        })
        .join()
        .unwrap();
        assert_eq!(result, Ok(11));
    }

    #[test]
    fn recv_with_timeout_send_wins_while_waiting() {
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
            rx.recv_with_timeout(Duration::from_millis(500))
        })
        .join()
        .unwrap();
        sender.join().unwrap();
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn recv_with_timeout_drop_sender_while_waiting() {
        use crate::sleep::sleep;

        let result = go!(move || {
            let (tx, rx) = channel::<i32>();
            go!(move || {
                sleep(Duration::from_millis(20));
                drop(tx);
            });
            rx.recv_with_timeout(Duration::from_secs(5))
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(RecvError::Disconnected));
    }

    #[test]
    fn recv_with_timeout_canceled_while_waiting() {
        use crate::sleep::sleep;

        let (_tx, rx) = channel::<i32>();
        let handle = go!(move || rx.recv_with_timeout(Duration::from_secs(5)));
        sleep(Duration::from_millis(50));
        unsafe {
            handle.coroutine().cancel();
        }
        assert!(handle.join().is_err());
    }

    #[test]
    fn recv_with_timeout_late_send_after_timeout() {
        use crate::sleep::sleep;

        go!(move || {
            let (tx, rx) = channel::<i32>();
            assert_eq!(
                rx.recv_with_timeout(Duration::from_millis(10)),
                Err(RecvError::Timeout)
            );
            go!(move || {
                sleep(Duration::from_millis(10));
                tx.send(99).unwrap();
            });
            sleep(Duration::from_millis(30));
            assert_eq!(rx.recv_with_timeout(Duration::from_secs(1)), Ok(99));
        });
    }

    #[test]
    fn recv_with_timeout_alternates_with_blocking_recv() {
        go!(move || {
            let (tx, rx) = channel::<i32>();
            assert_eq!(
                rx.recv_with_timeout(Duration::from_millis(10)),
                Err(RecvError::Timeout)
            );
            go!(move || {
                thread::sleep(Duration::from_millis(10));
                tx.send(1).unwrap();
            });
            assert_eq!(rx.recv().unwrap(), 1);
            assert_eq!(
                rx.recv_with_timeout(Duration::from_millis(10)),
                Err(RecvError::Timeout)
            );
        });
    }

    #[test]
    fn recv_with_timeout_stress_timeouts_and_sends() {
        use crate::sleep::sleep;

        let iterations = 50 * stress_factor();
        go!(move || {
            let (tx, rx) = channel::<i32>();
            for i in 0..iterations {
                assert_eq!(
                    rx.recv_with_timeout(Duration::from_millis(1)),
                    Err(RecvError::Timeout)
                );
                tx.send(i as i32).unwrap();
                assert_eq!(rx.recv_with_timeout(Duration::from_secs(1)), Ok(i as i32));
                if i % 10 == 0 {
                    sleep(Duration::from_millis(1));
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // recv_with_timeout tests — thread path
    // -----------------------------------------------------------------------

    #[test]
    fn recv_with_timeout_thread_immediate_data() {
        let (tx, rx) = channel::<i32>();
        thread::spawn(move || {
            tx.send(42).unwrap();
        });
        thread::sleep(Duration::from_millis(10));
        let result = rx.recv_with_timeout(Duration::from_secs(5));
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn recv_with_timeout_thread_timer_wins() {
        let (_tx, rx) = channel::<i32>();
        let start = Instant::now();
        let result = rx.recv_with_timeout(Duration::from_millis(50));
        let elapsed = start.elapsed();
        assert_eq!(result, Err(RecvError::Timeout));
        assert!(elapsed >= Duration::from_millis(40));
        assert!(elapsed < Duration::from_millis(200));
    }

    #[test]
    fn recv_with_timeout_thread_data_before_timeout() {
        let (tx, rx) = channel::<i32>();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            tx.send(99).unwrap();
        });
        let start = Instant::now();
        let result = rx.recv_with_timeout(Duration::from_secs(5));
        let elapsed = start.elapsed();
        handle.join().ok();
        assert_eq!(result, Ok(99));
        assert!(elapsed < Duration::from_millis(200));
    }

    #[test]
    fn recv_with_timeout_thread_disconnect() {
        let (tx, rx) = channel::<i32>();
        drop(tx);
        let result = rx.recv_with_timeout(Duration::from_secs(5));
        assert_eq!(result, Err(RecvError::Disconnected));
    }

    #[test]
    fn recv_with_timeout_thread_zero_duration_empty() {
        let (_tx, rx) = channel::<i32>();
        let start = Instant::now();
        let result = rx.recv_with_timeout(Duration::ZERO);
        let elapsed = start.elapsed();
        assert_eq!(result, Err(RecvError::Timeout));
        assert!(elapsed < Duration::from_millis(50));
    }

    #[test]
    fn recv_with_timeout_thread_zero_duration_with_data() {
        let (tx, rx) = channel::<i32>();
        tx.send(55).unwrap();
        assert_eq!(rx.recv_with_timeout(Duration::ZERO), Ok(55));
    }

    #[test]
    fn recv_with_timeout_thread_drop_sender_while_waiting() {
        let (tx, rx) = channel::<i32>();
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            drop(tx);
        });
        let result = rx.recv_with_timeout(Duration::from_secs(5));
        sender.join().unwrap();
        assert_eq!(result, Err(RecvError::Disconnected));
    }

    #[test]
    fn recv_with_timeout_thread_send_wins_while_waiting() {
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
        let result = rx.recv_with_timeout(Duration::from_millis(500));
        sender.join().unwrap();
        assert_eq!(result, Ok(88));
    }

    #[test]
    fn recv_with_timeout_thread_repeated() {
        let (tx, rx) = channel::<i32>();
        assert_eq!(
            rx.recv_with_timeout(Duration::from_millis(10)),
            Err(RecvError::Timeout)
        );
        tx.send(3).unwrap();
        assert_eq!(rx.recv_with_timeout(Duration::from_secs(1)), Ok(3));
        assert_eq!(
            rx.recv_with_timeout(Duration::from_millis(10)),
            Err(RecvError::Timeout)
        );
    }

    // -----------------------------------------------------------------------
    // Pre-existing spsc tests (from master) + recv_with_timeout tests above
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

    // #[test]
    // fn oneshot_single_thread_recv_timeout() {
    //     let (tx, rx) = channel();
    //     tx.send(()).unwrap();
    //     assert_eq!(rx.recv_timeout(Duration::from_millis(1)), Ok(()));
    //     assert_eq!(
    //         rx.recv_timeout(Duration::from_millis(1)),
    //         Err(RecvTimeoutError::Timeout)
    //     );
    //     tx.send(()).unwrap();
    //     assert_eq!(rx.recv_timeout(Duration::from_millis(1)), Ok(()));
    // }

    // #[test]
    // fn stress_recv_timeout_two_threads() {
    //     let (tx, rx) = channel();
    //     let stress = stress_factor() + 100;
    //     let timeout = Duration::from_millis(1);

    //     thread::spawn(move || {
    //         for i in 0..stress {
    //             if i % 2 == 0 {
    //                 thread::sleep(timeout * 2);
    //             }
    //             tx.send(1usize).unwrap();
    //         }
    //     });

    //     let mut recv_count = 0;
    //     loop {
    //         match rx.recv_timeout(timeout) {
    //             Ok(n) => {
    //                 assert_eq!(n, 1usize);
    //                 recv_count += 1;
    //             }
    //             Err(RecvTimeoutError::Timeout) => continue,
    //             Err(RecvTimeoutError::Disconnected) => break,
    //         }
    //     }

    //     assert_eq!(recv_count, stress);
    // }

    // #[test]
    // fn recv_timeout_upgrade() {
    //     let (_tx, rx) = channel::<()>();
    //     let timeout = Duration::from_millis(1);

    //     let start = Instant::now();
    //     assert_eq!(rx.recv_timeout(timeout), Err(RecvTimeoutError::Timeout));
    //     assert!(Instant::now() >= start + timeout);
    // }

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
    } /*
      }

      #[cfg(test)]
      mod sync_tests {
          use prelude::v1::*;

          use env;
          use thread;
          use super::*;
          use time::Duration;

          pub fn stress_factor() -> usize {
              match env::var("RUST_TEST_STRESS") {
                  Ok(val) => val.parse().unwrap(),
                  Err(..) => 1,
              }
          }

          #[test]
          fn smoke() {
              let (tx, rx) = sync_channel::<i32>(1);
              tx.send(1).unwrap();
              assert_eq!(rx.recv().unwrap(), 1);
          }

          #[test]
          fn drop_full() {
              let (tx, _rx) = sync_channel::<Box<isize>>(1);
              tx.send(box 1).unwrap();
          }

          #[test]
          fn smoke_shared() {
              let (tx, rx) = sync_channel::<i32>(1);
              tx.send(1).unwrap();
              assert_eq!(rx.recv().unwrap(), 1);
              let tx = tx.clone();
              tx.send(1).unwrap();
              assert_eq!(rx.recv().unwrap(), 1);
          }

          #[test]
          fn recv_timeout() {
              let (tx, rx) = sync_channel::<i32>(1);
              assert_eq!(rx.recv_timeout(Duration::from_millis(1)),
                         Err(RecvTimeoutError::Timeout));
              tx.send(1).unwrap();
              assert_eq!(rx.recv_timeout(Duration::from_millis(1)), Ok(1));
          }

          #[test]
          fn smoke_threads() {
              let (tx, rx) = sync_channel::<i32>(0);
              let _t = thread::spawn(move || {
                  tx.send(1).unwrap();
              });
              assert_eq!(rx.recv().unwrap(), 1);
          }

          #[test]
          fn smoke_port_gone() {
              let (tx, rx) = sync_channel::<i32>(0);
              drop(rx);
              assert!(tx.send(1).is_err());
          }

          #[test]
          fn smoke_shared_port_gone2() {
              let (tx, rx) = sync_channel::<i32>(0);
              drop(rx);
              let tx2 = tx.clone();
              drop(tx);
              assert!(tx2.send(1).is_err());
          }

          #[test]
          fn port_gone_concurrent() {
              let (tx, rx) = sync_channel::<i32>(0);
              let _t = thread::spawn(move || {
                  rx.recv().unwrap();
              });
              while tx.send(1).is_ok() {}
          }

          #[test]
          fn port_gone_concurrent_shared() {
              let (tx, rx) = sync_channel::<i32>(0);
              let tx2 = tx.clone();
              let _t = thread::spawn(move || {
                  rx.recv().unwrap();
              });
              while tx.send(1).is_ok() && tx2.send(1).is_ok() {}
          }

          #[test]
          fn smoke_chan_gone() {
              let (tx, rx) = sync_channel::<i32>(0);
              drop(tx);
              assert!(rx.recv().is_err());
          }

          #[test]
          fn smoke_chan_gone_shared() {
              let (tx, rx) = sync_channel::<()>(0);
              let tx2 = tx.clone();
              drop(tx);
              drop(tx2);
              assert!(rx.recv().is_err());
          }

          #[test]
          fn chan_gone_concurrent() {
              let (tx, rx) = sync_channel::<i32>(0);
              thread::spawn(move || {
                  tx.send(1).unwrap();
                  tx.send(1).unwrap();
              });
              while rx.recv().is_ok() {}
          }

          #[test]
          fn stress() {
              let (tx, rx) = sync_channel::<i32>(0);
              thread::spawn(move || {
                  for _ in 0..10000 {
                      tx.send(1).unwrap();
                  }
              });
              for _ in 0..10000 {
                  assert_eq!(rx.recv().unwrap(), 1);
              }
          }

          #[test]
          fn stress_recv_timeout_two_threads() {
              let (tx, rx) = sync_channel::<i32>(0);

              thread::spawn(move || {
                  for _ in 0..10000 {
                      tx.send(1).unwrap();
                  }
              });

              let mut recv_count = 0;
              loop {
                  match rx.recv_timeout(Duration::from_millis(1)) {
                      Ok(v) => {
                          assert_eq!(v, 1);
                          recv_count += 1;
                      }
                      Err(RecvTimeoutError::Timeout) => continue,
                      Err(RecvTimeoutError::Disconnected) => break,
                  }
              }

              assert_eq!(recv_count, 10000);
          }

          #[test]
          fn stress_recv_timeout_shared() {
              const AMT: u32 = 1000;
              const NTHREADS: u32 = 8;
              let (tx, rx) = sync_channel::<i32>(0);
              let (dtx, drx) = sync_channel::<()>(0);

              thread::spawn(move || {
                  let mut recv_count = 0;
                  loop {
                      match rx.recv_timeout(Duration::from_millis(10)) {
                          Ok(v) => {
                              assert_eq!(v, 1);
                              recv_count += 1;
                          }
                          Err(RecvTimeoutError::Timeout) => continue,
                          Err(RecvTimeoutError::Disconnected) => break,
                      }
                  }

                  assert_eq!(recv_count, AMT * NTHREADS);
                  assert!(rx.try_recv().is_err());

                  dtx.send(()).unwrap();
              });

              for _ in 0..NTHREADS {
                  let tx = tx.clone();
                  thread::spawn(move || {
                      for _ in 0..AMT {
                          tx.send(1).unwrap();
                      }
                  });
              }

              drop(tx);

              drx.recv().unwrap();
          }

          #[test]
          fn stress_shared() {
              const AMT: u32 = 1000;
              const NTHREADS: u32 = 8;
              let (tx, rx) = sync_channel::<i32>(0);
              let (dtx, drx) = sync_channel::<()>(0);

              thread::spawn(move || {
                  for _ in 0..AMT * NTHREADS {
                      assert_eq!(rx.recv().unwrap(), 1);
                  }
                  match rx.try_recv() {
                      Ok(..) => panic!(),
                      _ => {}
                  }
                  dtx.send(()).unwrap();
              });

              for _ in 0..NTHREADS {
                  let tx = tx.clone();
                  thread::spawn(move || {
                      for _ in 0..AMT {
                          tx.send(1).unwrap();
                      }
                  });
              }
              drop(tx);
              drx.recv().unwrap();
          }

          #[test]
          fn oneshot_single_thread_close_port_first() {
              // Simple test of closing without sending
              let (_tx, rx) = sync_channel::<i32>(0);
              drop(rx);
          }

          #[test]
          fn oneshot_single_thread_close_chan_first() {
              // Simple test of closing without sending
              let (tx, _rx) = sync_channel::<i32>(0);
              drop(tx);
          }

          #[test]
          fn oneshot_single_thread_send_port_close() {
              // Testing that the sender cleans up the payload if receiver is closed
              let (tx, rx) = sync_channel::<Box<i32>>(0);
              drop(rx);
              assert!(tx.send(box 0).is_err());
          }

          #[test]
          fn oneshot_single_thread_recv_chan_close() {
              // Receiving on a closed chan will panic
              let res = thread::spawn(move || {
                      let (tx, rx) = sync_channel::<i32>(0);
                      drop(tx);
                      rx.recv().unwrap();
                  })
                  .join();
              // What is our res?
              assert!(res.is_err());
          }

          #[test]
          fn oneshot_single_thread_send_then_recv() {
              let (tx, rx) = sync_channel::<Box<i32>>(1);
              tx.send(box 10).unwrap();
              assert!(rx.recv().unwrap() == box 10);
          }

          #[test]
          fn oneshot_single_thread_try_send_open() {
              let (tx, rx) = sync_channel::<i32>(1);
              assert_eq!(tx.try_send(10), Ok(()));
              assert!(rx.recv().unwrap() == 10);
          }

          // #[test]
          // fn oneshot_single_thread_try_send_closed() {
          //     let (tx, rx) = sync_channel::<i32>(0);
          //     drop(rx);
          //     assert_eq!(tx.try_send(10), Err(TrySendError::Disconnected(10)));
          // }

          // #[test]
          // fn oneshot_single_thread_try_send_closed2() {
          //     let (tx, _rx) = sync_channel::<i32>(0);
          //     assert_eq!(tx.try_send(10), Err(TrySendError::Full(10)));
          // }

          #[test]
          fn oneshot_single_thread_try_recv_open() {
              let (tx, rx) = sync_channel::<i32>(1);
              tx.send(10).unwrap();
              assert!(rx.recv() == Ok(10));
          }

          #[test]
          fn oneshot_single_thread_try_recv_closed() {
              let (tx, rx) = sync_channel::<i32>(0);
              drop(tx);
              assert!(rx.recv().is_err());
          }

          #[test]
          fn oneshot_single_thread_try_recv_closed_with_data() {
              let (tx, rx) = sync_channel::<i32>(1);
              tx.send(10).unwrap();
              drop(tx);
              assert_eq!(rx.try_recv(), Ok(10));
              assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
          }

          #[test]
          fn oneshot_single_thread_peek_data() {
              let (tx, rx) = sync_channel::<i32>(1);
              assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
              tx.send(10).unwrap();
              assert_eq!(rx.try_recv(), Ok(10));
          }

          #[test]
          fn oneshot_single_thread_peek_close() {
              let (tx, rx) = sync_channel::<i32>(0);
              drop(tx);
              assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
              assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
          }

          #[test]
          fn oneshot_single_thread_peek_open() {
              let (_tx, rx) = sync_channel::<i32>(0);
              assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
          }

          #[test]
          fn oneshot_multi_task_recv_then_send() {
              let (tx, rx) = sync_channel::<Box<i32>>(0);
              let _t = thread::spawn(move || {
                  assert!(rx.recv().unwrap() == box 10);
              });

              tx.send(box 10).unwrap();
          }

          #[test]
          fn oneshot_multi_task_recv_then_close() {
              let (tx, rx) = sync_channel::<Box<i32>>(0);
              let _t = thread::spawn(move || {
                  drop(tx);
              });
              let res = thread::spawn(move || {
                      assert!(rx.recv().unwrap() == box 10);
                  })
                  .join();
              assert!(res.is_err());
          }

          #[test]
          fn oneshot_multi_thread_close_stress() {
              for _ in 0..stress_factor() {
                  let (tx, rx) = sync_channel::<i32>(0);
                  let _t = thread::spawn(move || {
                      drop(rx);
                  });
                  drop(tx);
              }
          }

          #[test]
          fn oneshot_multi_thread_send_close_stress() {
              for _ in 0..stress_factor() {
                  let (tx, rx) = sync_channel::<i32>(0);
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
                  let (tx, rx) = sync_channel::<i32>(0);
                  let _t = thread::spawn(move || {
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
                  let (tx, rx) = sync_channel::<Box<i32>>(0);
                  let _t = thread::spawn(move || {
                      tx.send(box 10).unwrap();
                  });
                  assert!(rx.recv().unwrap() == box 10);
              }
          }

          // #[test]
          // fn stream_send_recv_stress() {
          //     for _ in 0..stress_factor() {
          //         let (tx, rx) = sync_channel::<Box<i32>>(0);
          //
          //         send(tx, 0);
          //         recv(rx, 0);
          //
          //         fn send(tx: SyncSender<Box<i32>>, i: i32) {
          //             if i == 10 {
          //                 return;
          //             }
          //
          //             thread::spawn(move || {
          //                 tx.send(box i).unwrap();
          //                 send(tx, i + 1);
          //             });
          //         }
          //
          //         fn recv(rx: Receiver<Box<i32>>, i: i32) {
          //             if i == 10 {
          //                 return;
          //             }
          //
          //             thread::spawn(move || {
          //                 assert!(rx.recv().unwrap() == box i);
          //                 recv(rx, i + 1);
          //             });
          //         }
          //     }
          // }

          #[test]
          fn recv_a_lot() {
              // Regression test that we don't run out of stack in scheduler context
              let (tx, rx) = sync_channel(10000);
              for _ in 0..10000 {
                  tx.send(()).unwrap();
              }
              for _ in 0..10000 {
                  rx.recv().unwrap();
              }
          }

          #[test]
          fn shared_chan_stress() {
              let (tx, rx) = sync_channel(0);
              let total = stress_factor() + 100;
              for _ in 0..total {
                  let tx = tx.clone();
                  thread::spawn(move || {
                      tx.send(()).unwrap();
                  });
              }

              for _ in 0..total {
                  rx.recv().unwrap();
              }
          }

          #[test]
          fn test_nested_recv_iter() {
              let (tx, rx) = sync_channel::<i32>(0);
              let (total_tx, total_rx) = sync_channel::<i32>(0);

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
              let (tx, rx) = sync_channel::<i32>(0);
              let (count_tx, count_rx) = sync_channel(0);

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
              let _ = tx.try_send(2);
              drop(tx);
              assert_eq!(count_rx.recv().unwrap(), 4);
          }

          #[test]
          fn try_recv_states() {
              let (tx1, rx1) = sync_channel::<i32>(1);
              let (tx2, rx2) = sync_channel::<()>(1);
              let (tx3, rx3) = sync_channel::<()>(1);
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
              let (tx, rx) = sync_channel::<()>(0);
              let (tx2, rx2) = sync_channel::<()>(0);
              let _t = thread::spawn(move || {
                  rx.recv().unwrap(); // wait on a oneshot
                  drop(rx);  // destroy a shared
                  tx2.send(()).unwrap();
              });
              // make sure the other thread has gone to sleep
              for _ in 0..5000 {
                  thread::yield_now();
              }

              // upgrade to a shared chan and send a message
              let t = tx.clone();
              drop(tx);
              t.send(()).unwrap();

              // wait for the child thread to exit before we exit
              rx2.recv().unwrap();
          }

          #[test]
          fn send1() {
              let (tx, rx) = sync_channel::<i32>(0);
              let _t = thread::spawn(move || {
                  rx.recv().unwrap();
              });
              assert_eq!(tx.send(1), Ok(()));
          }

          #[test]
          fn send2() {
              let (tx, rx) = sync_channel::<i32>(0);
              let _t = thread::spawn(move || {
                  drop(rx);
              });
              assert!(tx.send(1).is_err());
          }

          #[test]
          fn send3() {
              let (tx, rx) = sync_channel::<i32>(1);
              assert_eq!(tx.send(1), Ok(()));
              let _t = thread::spawn(move || {
                  drop(rx);
              });
              assert!(tx.send(1).is_err());
          }

          #[test]
          fn send4() {
              let (tx, rx) = sync_channel::<i32>(0);
              let tx2 = tx.clone();
              let (done, done_rx) = channel();
              let done2 = done.clone();
              let _t = thread::spawn(move || {
                  assert!(tx.send(1).is_err());
                  done.send(()).unwrap();
              });
              let _t = thread::spawn(move || {
                  assert!(tx2.send(2).is_err());
                  done2.send(()).unwrap();
              });
              drop(rx);
              done_rx.recv().unwrap();
              done_rx.recv().unwrap();
          }

          // #[test]
          // fn try_send1() {
          //     let (tx, _rx) = sync_channel::<i32>(0);
          //     assert_eq!(tx.try_send(1), Err(TrySendError::Full(1)));
          // }

          // #[test]
          // fn try_send2() {
          //     let (tx, _rx) = sync_channel::<i32>(1);
          //     assert_eq!(tx.try_send(1), Ok(()));
          //     assert_eq!(tx.try_send(1), Err(TrySendError::Full(1)));
          // }

          // #[test]
          // fn try_send3() {
          //     let (tx, rx) = sync_channel::<i32>(1);
          //     assert_eq!(tx.try_send(1), Ok(()));
          //     drop(rx);
          //     assert_eq!(tx.try_send(1), Err(TrySendError::Disconnected(1)));
          // }

          #[test]
          fn issue_15761() {
              fn repro() {
                  let (tx1, rx1) = sync_channel::<()>(3);
                  let (tx2, rx2) = sync_channel::<()>(3);

                  let _t = thread::spawn(move || {
                      rx1.recv().unwrap();
                      tx2.try_send(()).unwrap();
                  });

                  tx1.try_send(()).unwrap();
                  rx2.recv().unwrap();
              }

              for _ in 0..100 {
                  repro()
              }
          }

          #[test]
          fn fmt_debug_sender() {
              let (tx, _) = channel::<i32>();
              assert_eq!(format!("{:?}", tx), "Sender { .. }");
          }

          #[test]
          fn fmt_debug_recv() {
              let (_, rx) = channel::<i32>();
              assert_eq!(format!("{:?}", rx), "Receiver { .. }");
          }

          // #[test]
          // fn fmt_debug_sync_sender() {
          //     let (tx, _) = sync_channel::<i32>(1);
          //     assert_eq!(format!("{:?}", tx), "SyncSender { .. }");
          // }*/
}
