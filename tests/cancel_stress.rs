//! Stress tests for `Coroutine::cancel` exercised from safe code.
//!
//! Meant to be run on Windows to exercise the IOCP-backed cancel path (raw
//! `CancelIoEx` on a stack-resident `EventData`), the timer path and the park
//! path, including concurrent and repeated cancels.

#[cfg(feature = "io_cancel")]
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[macro_use]
extern crate may;

use generator::Error;
use may::coroutine;
#[cfg(feature = "io_cancel")]
use may::net::TcpListener;
use may::sync::mpsc::channel;
use may::sync::{Condvar, Mutex};

fn is_cancel(err: &(dyn std::any::Any + Send)) -> bool {
    matches!(err.downcast_ref::<Error>(), Some(Error::Cancel))
}

fn wait_a_bit() {
    thread::sleep(Duration::from_millis(2));
}

/// Cancel a coroutine suspended on `coroutine::park()`.
#[test]
fn cancel_parked() {
    for _ in 0..200 {
        let (tx, rx) = channel();
        let j = go!(move || {
            tx.send(()).unwrap();
            coroutine::park();
            unreachable!("park() returned after cancel");
        });
        rx.recv().unwrap();
        wait_a_bit();
        j.coroutine().cancel();
        assert!(j.join().is_err_and(|e| is_cancel(&*e)));
    }
}

/// Cancel a coroutine suspended in `coroutine::sleep()` (timer path).
#[test]
fn cancel_sleeping() {
    for _ in 0..200 {
        let (tx, rx) = channel();
        let j = go!(move || {
            tx.send(()).unwrap();
            coroutine::sleep(Duration::from_secs(3600));
            unreachable!("sleep() returned after cancel");
        });
        rx.recv().unwrap();
        wait_a_bit();
        j.coroutine().cancel();
        assert!(j.join().is_err_and(|e| is_cancel(&*e)));
    }
}

/// Cancel a coroutine that is actively running (yield loop).
#[test]
fn cancel_running() {
    for _ in 0..100 {
        let (tx, rx) = channel();
        let j = go!(move || {
            tx.send(()).unwrap();
            loop {
                coroutine::yield_now();
            }
        });
        rx.recv().unwrap();
        thread::sleep(Duration::from_micros(200));
        j.coroutine().cancel();
        assert!(j.join().is_err_and(|e| is_cancel(&*e)));
    }
}

/// Cancel from several threads at once; only one of them can win.
#[test]
fn cancel_concurrently() {
    for _ in 0..100 {
        let (tx, rx) = channel();
        let j = go!(move || {
            tx.send(()).unwrap();
            coroutine::park();
            unreachable!("park() returned after cancel");
        });
        rx.recv().unwrap();
        wait_a_bit();

        let co = j.coroutine().clone();
        let hs: Vec<_> = (0..4)
            .map(|_| {
                let co = co.clone();
                thread::spawn(move || co.cancel())
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert!(j.join().is_err_and(|e| is_cancel(&*e)));
    }
}

/// Cancellation must unwind the coroutine stack exactly once.
#[test]
fn cancel_runs_drop_once() {
    struct Guard(Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    for _ in 0..200 {
        let drops = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = channel();
        let d = drops.clone();
        let j = go!(move || {
            let _g = Guard(d);
            tx.send(()).unwrap();
            coroutine::park();
            unreachable!();
        });
        rx.recv().unwrap();
        wait_a_bit();
        j.coroutine().cancel();
        assert!(j.join().is_err_and(|e| is_cancel(&*e)));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

/// Cancel a coroutine blocked in `accept()` (IOCP cancel path).
#[cfg(feature = "io_cancel")]
#[test]
fn cancel_accept() {
    for _ in 0..200 {
        let (tx, rx) = channel();
        let j = go!(move || {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let _ = listener.accept();
            unreachable!("accept() returned after cancel");
        });
        let _addr = rx.recv().unwrap();
        wait_a_bit();
        j.coroutine().cancel();
        assert!(j.join().is_err_and(|e| is_cancel(&*e)));
    }
}

/// Cancel a coroutine blocked in `read()` while the peer sends data at the
/// same time, racing a normal IO completion against the cancel.
#[cfg(feature = "io_cancel")]
#[test]
fn cancel_read_racing_completion() {
    for i in 0..300 {
        let (tx, rx) = channel();
        let (accepted_tx, accepted_rx) = channel();
        let j = go!(move || {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let (mut s, _) = listener.accept().unwrap();
            accepted_tx.send(()).unwrap();
            let mut buf = [0u8; 64];
            loop {
                let _ = s.read(&mut buf);
                coroutine::yield_now();
            }
        });

        let addr = rx.recv().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        accepted_rx.recv().unwrap();

        let co = j.coroutine().clone();
        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_micros((i % 400) as u64));
            let _ = client.write_all(b"ping");
            // keep the connection open until the cancel has resolved
            thread::sleep(Duration::from_millis(5));
        });

        // race the cancel against the completion
        thread::sleep(Duration::from_micros((i % 300) as u64));
        co.cancel();
        writer.join().unwrap();
        assert!(j.join().is_err_and(|e| is_cancel(&*e)));
    }
}

/// Repeated cancel after the coroutine already finished must be a no-op.
#[test]
fn cancel_after_finish() {
    for _ in 0..200 {
        let j = go!(|| 42usize);
        let co = j.coroutine().clone();
        assert_eq!(j.join().unwrap(), 42);
        co.cancel();
        co.cancel();
    }
}

/// Cancel a coroutine that has no cancellation-aware point left (busy
/// running): the cancellation must still be reported from `join` instead of
/// silently returning the coroutine's result.
///
/// The coroutine keeps spinning on a plain atomic until the main thread has
/// delivered the cancel, so the cancel is guaranteed to land while the
/// coroutine is still running. A fixed-iteration busy loop is a race here:
/// in release builds it finishes faster than the main thread wakes up from
/// the channel receive, making the cancel a no-op after completion.
#[test]
fn cancel_reported_at_completion() {
    for _ in 0..100 {
        let (tx, rx) = channel();
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let j = go!(move || {
            tx.send(()).unwrap();
            // busy work with no suspension points; keep spinning until the
            // main thread has delivered the cancel
            let mut x = 0usize;
            while r.load(Ordering::Acquire) {
                x = x.wrapping_add(1);
                std::hint::spin_loop();
            }
            x
        });
        rx.recv().unwrap();
        // the coroutine cannot finish before this: `running` is only
        // cleared after the cancel flag is set
        j.coroutine().cancel();
        running.store(false, Ordering::Release);
        assert!(j.join().is_err_and(|e| is_cancel(&*e)));
    }
}

/// Cancel a coroutine while it is parked on a `Mutex` with cancellation
/// *disabled* — the state `Condvar::wait_impl` enters for its internal
/// re-lock — must not corrupt the mutex's waiter count.
///
/// The first cancel wakes the coroutine from the condvar wait; it then
/// re-locks the mutex with cancel disabled. The second cancel delivers a
/// fresh `Canceled` into that mutex wait, exercising the deferred-cancel
/// (`b_ignore`) path: the wait must keep waiting and the mutex must remain
/// usable afterwards. Do NOT remove the second cancel — it is the trigger.
#[test]
fn cancel_disabled_while_parked_on_mutex() {
    let pair = Arc::new((Mutex::new(0usize), Condvar::new()));

    let p = pair.clone();
    let h = go!(move || {
        let (m, c) = &*p;
        let g = m.lock().unwrap();
        // waits on the condvar, releasing the mutex
        let _ = c.wait(g);
        unreachable!("wait() returned after cancel");
    });

    // let the coroutine reach the condvar wait
    thread::sleep(Duration::from_millis(100));

    // hold the mutex so the coroutine's re-lock must park
    let guard = pair.0.lock().unwrap();

    // first cancel: the coroutine wakes from the condvar wait,
    // disables cancel and parks on the mutex re-lock
    h.coroutine().cancel();
    thread::sleep(Duration::from_millis(200));

    // second cancel: lands while parked on the mutex with cancel disabled
    h.coroutine().cancel();
    thread::sleep(Duration::from_millis(200));

    // hand the mutex over to the coroutine
    drop(guard);
    assert!(h.join().is_err_and(|e| is_cancel(&*e)));

    // the mutex must be free and usable afterwards
    let g = pair
        .0
        .try_lock()
        .expect("mutex corrupted after cancel: try_lock failed on a free mutex");
    drop(g);
}
