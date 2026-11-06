//! Stress tests for `Coroutine::cancel` exercised from safe code.
//!
//! Meant to be run on Windows to exercise the IOCP-backed cancel path (raw
//! `CancelIoEx` on a stack-resident `EventData`), the timer path and the park
//! path, including concurrent and repeated cancels.

#[cfg(feature = "io_cancel")]
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
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
