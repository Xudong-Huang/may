//! Multi-producer, multi-consumer channels for threads and coroutines.
//!
//! Unlike [`super::mpsc`], every receiver consumes at most one value per
//! receive, and both [`Sender`] and [`Receiver`] are `Clone` and `Sync`, so
//! they can be shared across threads and coroutines. The channel is
//! unbounded: `send` never blocks, while receivers block when the queue is
//! empty.
//!
//! Disconnect semantics match `std::sync::mpsc`: dropping every sender
//! disconnects the channel, and values already queued are still delivered
//! before `try_recv`/`recv` report `Disconnected`.
//!
//! For coroutines, a blocking receive is a cancellation point: cancellation
//! unwinds the coroutine with the cancel panic instead of returning an error.

use std::fmt;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
// Re-exported so the error types are reachable through this module's path,
// matching the other channel modules.
pub use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use super::semphore::WaitStatus;
use super::Semaphore;
use crossbeam::queue::SegQueue;

/// /////////////////////////////////////////////////////////////////////////////
/// InnerQueue
/// /////////////////////////////////////////////////////////////////////////////
struct InnerQueue<T> {
    queue: SegQueue<T>,
    // thread/coroutine for wake up
    sem: Semaphore,
    // The number of tx channels which are currently using this queue.
    tx_ports: AtomicUsize,
    // if rx is dropped
    rx_ports: AtomicUsize,
}

impl<T> InnerQueue<T> {
    pub fn new() -> InnerQueue<T> {
        InnerQueue {
            queue: SegQueue::new(),
            sem: Semaphore::new(0),
            tx_ports: AtomicUsize::new(1),
            rx_ports: AtomicUsize::new(1),
        }
    }

    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        if self.rx_ports.load(Ordering::Acquire) == 0 {
            return Err(SendError(t));
        }

        self.queue.push(t);
        self.sem.post();
        Ok(())
    }

    pub fn recv(&self, dur: Option<Duration>) -> Result<T, RecvTimeoutError> {
        match self.try_recv() {
            Ok(data) => return Ok(data),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
        }

        // The disconnect re-check is done *after* the waiter is published
        // (inside `wait_timeout_until`): a receiver that parks after the last
        // sender's wakeup would otherwise never be woken again.
        match self
            .sem
            .wait_timeout_until(dur, || self.tx_ports.load(Ordering::Acquire) == 0)
        {
            WaitStatus::Acquired => {}
            WaitStatus::Stopped => {
                // The wait ended on the disconnect. A sender may still have
                // published its last value before dropping its port, and the
                // disconnect wake-up is only advisory, so `Stopped` does not
                // imply that no value is buffered. Queued values must be
                // delivered before the disconnect is reported.
                return match self.queue.pop() {
                    Some(data) => Ok(data),
                    None => Err(RecvTimeoutError::Disconnected),
                };
            }
            WaitStatus::Timeout => return Err(RecvTimeoutError::Timeout),
        }

        match self.queue.pop() {
            Some(data) => Ok(data),
            None => match self.tx_ports.load(Ordering::Acquire) {
                // a disconnect wakeup has no item; this is the normal
                // disconnect path. The queue is final once the port count
                // reads 0, so retry the pop: the first one may have raced
                // with the last publish.
                0 => match self.queue.pop() {
                    Some(data) => Ok(data),
                    None => Err(RecvTimeoutError::Disconnected),
                },
                _ => {
                    // A permit with no item while senders are alive means the
                    // semaphore/queue accounting diverged. Report the channel
                    // as disconnected in release builds instead of panicking
                    // from a library receive path.
                    debug_assert!(false, "mpmc: a semaphore permit had no queued item");
                    Err(RecvTimeoutError::Disconnected)
                }
            },
        }
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        if !self.sem.try_wait() {
            return match self.tx_ports.load(Ordering::Acquire) {
                // Disconnected: a sender may have published its last value
                // between the failed `try_wait` and this load, so check the
                // queue before reporting the disconnect. Once the port count
                // reads 0, no further value can be published.
                0 => match self.queue.pop() {
                    Some(data) => Ok(data),
                    None => Err(TryRecvError::Disconnected),
                },
                _ => Err(TryRecvError::Empty),
            };
        }

        match self.queue.pop() {
            Some(data) => Ok(data),
            None => match self.tx_ports.load(Ordering::Acquire) {
                // the queue is final once the port count reads 0, so retry
                // the pop before reporting the disconnect: the first one may
                // have raced with the last publish
                0 => match self.queue.pop() {
                    Some(data) => Ok(data),
                    None => Err(TryRecvError::Disconnected),
                },
                _ => {
                    debug_assert!(false, "mpmc: a semaphore permit had no queued item");
                    Err(TryRecvError::Empty)
                }
            },
        }
    }

    pub fn clone_tx(&self) {
        self.tx_ports.fetch_add(1, Ordering::SeqCst);
    }

    pub fn drop_tx(&self) {
        match self.tx_ports.fetch_sub(1, Ordering::SeqCst) {
            1 => {
                // There is no tx port any more; wake every parked receiver
                // so it observes the disconnect. This must be a broadcast,
                // not a single wake: the stop predicate is permanently true
                // and every receiver has to re-check it.
                //
                // Receivers that register after the drain observe
                // `tx_ports == 0` through `wait_timeout_until`'s
                // post-registration check, so no wakeup is lost.
                self.sem.wake_all();
            }
            n if n > 1 => {}
            n => {
                // underflow: an internal accounting bug. Do not panic from a
                // Drop implementation (it can abort during unwinding).
                debug_assert!(false, "bad number of tx_ports left {n}");
            }
        }
    }

    pub fn clone_rx(&self) {
        self.rx_ports.fetch_add(1, Ordering::SeqCst);
    }

    pub fn drop_rx(&self) {
        match self.rx_ports.fetch_sub(1, Ordering::SeqCst) {
            1 => {
                // there is no receiver any more, clear the data
                while self.queue.pop().is_some() {}
            }
            n if n > 1 => {}
            n => {
                debug_assert!(false, "bad number of rx_ports left {n}");
            }
        }
    }
}

impl<T> Drop for InnerQueue<T> {
    fn drop(&mut self) {
        // invariant checks only: panicking in Drop while unwinding aborts
        debug_assert_eq!(self.tx_ports.load(Ordering::Acquire), 0);
        debug_assert_eq!(self.rx_ports.load(Ordering::Acquire), 0);
    }
}

/// The receiving half of an MPMC channel created by [`channel`].
///
/// Receivers are cloneable and shareable: each `recv`/`try_recv` consumes at
/// most one queued value, and concurrent receivers never observe the same
/// value.
pub struct Receiver<T> {
    inner: Arc<InnerQueue<T>>,
}

unsafe impl<T: Send> Send for Receiver<T> {}
// MPMC halves are intentionally `Sync` (unlike `mpsc`); values are owned by
// the queue and only moved out.
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

/// The sending half of an MPMC channel created by [`channel`].
///
/// Cloning a `Sender` creates another handle to the same channel. Senders
/// never block: the channel is unbounded.
pub struct Sender<T> {
    inner: Arc<InnerQueue<T>>,
}

unsafe impl<T: Send> Send for Sender<T> {}
impl<T> UnwindSafe for Sender<T> {}
impl<T> RefUnwindSafe for Sender<T> {}

/// Create an unbounded multi-producer, multi-consumer channel.
///
/// Both halves are cloneable and can be shared between threads or coroutines.
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

    /// Send a value on the channel, returning it back on error.
    ///
    /// Returns `Err` if every receiver has been dropped. Sending never
    /// blocks: the channel is unbounded.
    pub fn send(&self, t: T) -> Result<(), SendError<T>> {
        self.inner.send(t)
    }

    /// Return a best-effort count of queued items not yet claimed by a
    /// receiver.
    ///
    /// This is the semaphore's permit count: one permit is posted per send
    /// and consumed by `try_wait` when a receiver pops a value, so it can
    /// transiently over-report a value whose receiver has been woken but has
    /// not acquired the permit yet.
    pub fn pressure(&self) -> usize {
        self.inner.sem.get_value()
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Sender<T> {
        self.inner.clone_tx();
        Sender::new(self.inner.clone())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.inner.drop_tx();
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

    /// Attempt to receive a value without blocking.
    ///
    /// Returns [`TryRecvError::Empty`] if the queue is empty and at least one
    /// sender is alive, and [`TryRecvError::Disconnected`] if every sender has
    /// been dropped and the queue is empty.
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
        match self.inner.recv(None) {
            Err(RecvTimeoutError::Timeout) => {
                debug_assert!(false, "mpmc recv(None) returned a timeout");
                Err(RecvError)
            }
            data => data.map_err(|_| RecvError),
        }
    }

    /// Block until a value is available, the timeout expires, or the channel
    /// is disconnected.
    ///
    /// # Panics
    ///
    /// In a coroutine context, panics if the coroutine is canceled while
    /// waiting.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        self.inner.recv(Some(timeout))
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

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Receiver<T> {
        self.inner.clone_rx();
        Receiver::new(self.inner.clone())
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.inner.drop_rx();
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
        use crate::sync::mpmc::{
            RecvError, RecvTimeoutError, SendError, TryRecvError, TrySendError,
        };
        let _: Option<RecvError> = None;
        let _: Option<RecvTimeoutError> = None;
        let _: Option<SendError<()>> = None;
        let _: Option<TryRecvError> = None;
        let _: Option<TrySendError<()>> = None;

        // all three iterators implement Debug, like mpsc/std
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_debug::<Iter<'static, i32>>();
        assert_debug::<TryIter<'static, i32>>();
        assert_debug::<IntoIter<i32>>();

        // both halves are unwind-safe regardless of T
        fn assert_unwind<T: std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
        assert_unwind::<Sender<*mut ()>>();
        assert_unwind::<Receiver<*mut ()>>();

        // unlike mpsc, both halves are cloneable and shareable
        fn assert_clone_sync<T: Clone + Sync>() {}
        assert_clone_sync::<Sender<i32>>();
        assert_clone_sync::<Receiver<i32>>();
    }

    /// A coroutine blocked in `recv()` must be woken by cancellation.
    #[test]
    fn recv_canceled_while_waiting() {
        use crate::sleep::sleep;
        use std::time::Instant;

        let (_tx, rx) = channel::<i32>();
        let h = go!(move || {
            let _ = rx.recv();
        });
        sleep(Duration::from_millis(50));
        unsafe { h.coroutine().cancel() };

        let start = Instant::now();
        while !h.is_done() && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            h.is_done(),
            "cancel did not wake a coroutine blocked in recv()"
        );
        assert!(h.join().is_err());
    }

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
        drop(tx.clone());
        drop(tx.clone());
        tx.send(Box::new(1)).unwrap();
    }

    #[test]
    fn smoke_shared() {
        let (tx, rx) = channel::<i32>();
        tx.send(1).unwrap();
        assert_eq!(rx.recv().unwrap(), 1);
        let tx = tx.clone();
        tx.send(1).unwrap();
        assert_eq!(rx.recv().unwrap(), 1);
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
    fn smoke_shared_port_gone2() {
        let (tx, rx) = channel::<i32>();
        drop(rx);
        let tx2 = tx.clone();
        drop(tx);
        assert!(tx2.send(1).is_err());
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
    fn port_gone_concurrent_shared() {
        let (tx, rx) = channel::<i32>();
        let tx2 = tx.clone();
        let _t = thread::spawn(move || {
            rx.recv().unwrap();
        });
        while tx.send(1).is_ok() && tx2.send(1).is_ok() {}
    }

    #[test]
    fn smoke_chan_gone() {
        let (tx, rx) = channel::<i32>();
        drop(tx);
        assert!(rx.recv().is_err());
    }

    #[test]
    fn smoke_chan_gone_shared() {
        let (tx, rx) = channel::<()>();
        let tx2 = tx.clone();
        drop(tx);
        drop(tx2);
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
    fn stress_shared() {
        const AMT: u32 = 10000;
        const NTHREADS: u32 = 8;
        let (tx, rx) = channel::<i32>();

        let t = thread::spawn(move || {
            for _ in 0..AMT * NTHREADS {
                assert_eq!(rx.recv().unwrap(), 1);
            }
            if rx.try_recv().is_ok() {
                panic!();
            }
        });

        for _ in 0..NTHREADS {
            let tx = tx.clone();
            go!(move || for _ in 0..AMT {
                tx.send(1).unwrap();
            });
        }
        drop(tx);
        t.join().ok().unwrap();
    }

    #[test]
    fn send_from_outside_runtime() {
        let (tx1, rx1) = channel::<()>();
        let (tx2, rx2) = channel::<i32>();
        let t1 = go!(move || {
            tx1.send(()).unwrap();
            for _ in 0..40 {
                let r = rx2.recv();
                assert_eq!(r.unwrap(), 1);
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
        let (tx, rx) = channel::<()>();
        let timeout = Duration::from_millis(1);
        let _tx_clone = tx.clone();

        // let start = Instant::now();
        // let end = start + timeout;
        assert_eq!(rx.recv_timeout(timeout), Err(RecvTimeoutError::Timeout));
        // let now = Instant::now();
        // println!("now = {:?}, end={:?}", now, end);
        // assert!(Instant::now() >= end);
    }

    #[test]
    fn stress_recv_timeout_shared() {
        let (tx, rx) = channel();
        let stress = stress_factor() + 100;

        for i in 0..stress {
            let tx = tx.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(i as u64 * 10));
                tx.send(1usize).unwrap();
            });
        }

        drop(tx);

        let mut recv_count = 0;
        loop {
            match rx.recv_timeout(Duration::from_millis(30)) {
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
    fn shared_recv_timeout() {
        let (tx, rx) = channel();
        let total = 5;
        for _ in 0..total {
            let tx = tx.clone();
            thread::spawn(move || {
                tx.send(()).unwrap();
            });
        }

        for _ in 0..total {
            rx.recv().unwrap();
        }

        assert_eq!(
            rx.recv_timeout(Duration::from_millis(1)),
            Err(RecvTimeoutError::Timeout)
        );
        tx.send(()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_millis(1)), Ok(()));
    }

    #[test]
    fn shared_chan_stress() {
        let (tx, rx) = channel();
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

    /// A value published before the last sender dropped must still be
    /// delivered when the receiver's permit attempt raced with the
    /// disconnect: reporting `Disconnected` with the value still queued
    /// loses it for a single receiver.
    #[test]
    fn disconnect_does_not_hide_a_queued_value() {
        let (tx, rx) = channel::<i32>();
        tx.send(1).unwrap();
        // Simulate a receiver that raced with the final publish: the permit
        // was consumed (a hand-off that lost to a waiter leaving) while the
        // value stays queued.
        assert!(rx.inner.sem.try_wait());
        drop(tx);

        assert_eq!(rx.try_recv(), Ok(1));
        assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
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

        // upgrade to a shared chan and send a message
        let t = tx.clone();
        drop(tx);
        t.send(()).unwrap();

        // wait for the child thread to exit before we exit
        rx2.recv().unwrap();
    }

    #[test]
    fn stress_multi_recv() {
        use crate::sync::mpsc;
        let (tx, rx) = channel();
        let (tx1, rx1) = mpsc::channel();
        let stress = stress_factor() + 100;

        for i in 0..10 {
            let rx = rx.clone();
            let tx = tx1.clone();
            thread::spawn(move || {
                while let Ok(v) = rx.recv() {
                    tx.send((i, v)).unwrap();
                }
            });
        }

        for i in 0..stress {
            tx.send(i).unwrap();
        }

        // wait for done
        let mut recv_count = 0;
        while let Ok((id, v)) = rx1.recv() {
            println!("recv from id={id}, value={v}");
            recv_count += 1;

            if recv_count == stress {
                break;
            }
        }
        assert!(rx1.try_recv().is_err());
    }

    /// Every receiver must observe the disconnect, including one that registers
    /// for the semaphore after the last sender's wakeup posts have run.
    ///
    /// Regression: a single spare permit was left by `drop_tx`, so if another
    /// receiver consumed it first, the late receiver parked forever.
    #[test]
    fn multi_receiver_disconnect_wakeup() {
        use std::time::Instant;

        const N: usize = 8;
        for iter in 0..100 {
            let (tx, rx) = channel::<i32>();
            let done = Arc::new(AtomicUsize::new(0));
            let mut handles = Vec::new();
            for _ in 0..N {
                let rx = rx.clone();
                let done = done.clone();
                handles.push(thread::spawn(move || {
                    let _ = rx.recv();
                    done.fetch_add(1, Ordering::SeqCst);
                }));
            }
            drop(tx);

            let start = Instant::now();
            while done.load(Ordering::SeqCst) < N && start.elapsed() < Duration::from_secs(2) {
                thread::sleep(Duration::from_millis(1));
            }
            let n = done.load(Ordering::SeqCst);
            assert_eq!(
                n, N,
                "iter {iter}: only {n}/{N} receivers woke on disconnect"
            );
            for h in handles {
                h.join().unwrap();
            }
        }
    }
}
