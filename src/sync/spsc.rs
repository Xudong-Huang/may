//! compatible with std::sync::mpsc except for both thread and coroutine
//! please ref the doc from std::sync::mpsc
use std::fmt;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{RecvError, RecvTimeoutError, SendError, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{AtomicOption, Blocker};
use may_queue::spsc::Queue as WaitList;
// TODO: SyncSender
/// /////////////////////////////////////////////////////////////////////////////
/// InnerQueue
/// /////////////////////////////////////////////////////////////////////////////
struct InnerQueue<T> {
    // data
    queue: WaitList<T>,
    // thread/coroutine for wake up the receiver
    to_wake: AtomicOption<Arc<Blocker>>,
    // The number of tx channels which are currently using this queue.
    channels: AtomicUsize,
    // if rx is dropped
    port_dropped: AtomicBool,
}

impl<T> InnerQueue<T> {
    pub fn new() -> InnerQueue<T> {
        InnerQueue {
            queue: WaitList::new(),
            to_wake: AtomicOption::none(),
            channels: AtomicUsize::new(1),
            port_dropped: AtomicBool::new(false),
        }
    }

    pub fn send(&self, t: T) -> Result<(), T> {
        if self.port_dropped.load(Ordering::Acquire) {
            return Err(t);
        }
        self.queue.push(t);
        if let Some(w) = self.to_wake.take(Ordering::Acquire) {
            w.unpark();
        }
        Ok(())
    }

    pub fn recv(&self, dur: Option<Duration>) -> Result<T, TryRecvError> {
        match self.try_recv() {
            Err(TryRecvError::Empty) => {}
            data => return data,
        }

        let cur = Blocker::current();
        // register the waiter
        self.to_wake.swap(cur.clone(), Ordering::Release);
        // re-check the queue
        match self.try_recv() {
            Err(TryRecvError::Empty) => {
                cur.park(dur).ok();
            }
            data => {
                // no need to park, contention with send
                if let Some(w) = self.to_wake.take(Ordering::Acquire) {
                    w.unpark();
                }
                cur.park(dur).ok();
                return data;
            }
        }

        // after come back try recv again
        self.try_recv()
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        match self.queue.pop() {
            Some(data) => Ok(data),
            None => {
                match self.channels.load(Ordering::Acquire) {
                    // there is no sender any more, should re-check
                    0 => self.queue.pop().ok_or_else(|| TryRecvError::Disconnected),
                    _ => Err(TryRecvError::Empty),
                }
            }
        }
    }

    pub fn drop_chan(&self) {
        match self.channels.fetch_sub(1, Ordering::AcqRel) {
            1 => self
                .to_wake
                .take(Ordering::Relaxed)
                .map(|w| w.unpark())
                .unwrap_or(()),
            n if n > 1 => {}
            n => panic!("bad number of channels left {}", n),
        }
    }

    pub fn drop_port(&self) {
        self.port_dropped.store(true, Ordering::Release);
        // clear all the data
        while let Some(_) = self.queue.pop() {}
    }
}

impl<T> Drop for InnerQueue<T> {
    fn drop(&mut self) {
        assert_eq!(self.channels.load(Ordering::Acquire), 0);
        assert_eq!(self.to_wake.is_none(), true);
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

/// /////////////////////////////////////////////////////////////////////////////
/// Sender
/// /////////////////////////////////////////////////////////////////////////////

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

/// /////////////////////////////////////////////////////////////////////////////
/// Receiver
/// /////////////////////////////////////////////////////////////////////////////

impl<T> Receiver<T> {
    fn new(inner: Arc<InnerQueue<T>>) -> Receiver<T> {
        Receiver { inner }
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.inner.try_recv()
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        loop {
            match self.inner.recv(None) {
                Err(TryRecvError::Empty) => {}
                data => return data.map_err(|_| RecvError),
            }
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        // Do an optimistic try_recv to avoid the performance impact of
        // Instant::now() in the full-channel case.
        match self.try_recv() {
            Ok(result) => Ok(result),
            Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
            Err(TryRecvError::Empty) => self.recv_max_until(timeout),
        }
    }

    fn recv_max_until(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.inner.recv(Some(timeout)) {
                Ok(t) => return Ok(t),
                Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
                Err(TryRecvError::Empty) => {}
            }

            // If we're already passed the deadline, and we're here without
            // data, return a timeout, else try again.
            if Instant::now() >= deadline {
                return Err(RecvTimeoutError::Timeout);
            }
        }
    }

    pub fn iter(&self) -> Iter<T> {
        Iter { rx: self }
    }

    pub fn try_iter(&self) -> TryIter<T> {
        TryIter { rx: self }
    }
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.rx.recv().ok()
    }
}

impl<'a, T> Iterator for TryIter<'a, T> {
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
    use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
    use std::thread;
    use std::time::Duration;

    pub fn stress_factor() -> usize {
        match env::var("RUST_TEST_STRESS") {
            Ok(val) => val.parse().unwrap(),
            Err(..) => 1,
        }
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
        assert_eq!(iter.next().is_none(), true);
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
        assert_eq!(iter.next().is_none(), true);
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
}
