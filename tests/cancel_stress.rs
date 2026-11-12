//! Stress tests for the unsafe `Coroutine::cancel`.
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
use may::sync::spsc;
use may::sync::{Condvar, Mutex, RwLock};

fn is_cancel(err: &(dyn std::any::Any + Send)) -> bool {
    matches!(err.downcast_ref::<Error>(), Some(Error::Cancel))
}

fn wait_a_bit() {
    thread::sleep(Duration::from_millis(2));
}

/// A wake-up that races with `recv()`'s park registration must not resume
/// the coroutine inline: the resumed coroutine can return from `recv()`,
/// drop the stack-resident wait source while its `subscribe` is still on the
/// stack, and then `DelayDrop::wait` cannot yield because the coroutine is
/// cancelled.
#[test]
fn cancel_racing_handoff_is_not_lost_on_spsc_recv() {
    for round in 0..2000usize {
        let (tx, rx) = spsc::channel::<usize>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let sender = thread::spawn(move || {
            let mut i = 0usize;
            while !stop2.load(Ordering::Relaxed) {
                if tx.send(i).is_err() {
                    return;
                }
                i += 1;
                thread::sleep(Duration::from_micros(20));
            }
        });

        let h = go!(move || loop {
            if rx.recv().is_err() {
                return;
            }
        });

        // cancel at varying points while the receiver parks and wakes
        thread::sleep(Duration::from_micros((round % 61) as u64 + 1));
        unsafe { h.coroutine().cancel() };
        assert!(h.join().is_err(), "recv() did not observe the cancel");
        stop.store(true, Ordering::Relaxed);
        sender.join().unwrap();
    }
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
        unsafe { j.coroutine().cancel() };
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
        unsafe { j.coroutine().cancel() };
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
        unsafe { j.coroutine().cancel() };
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
                thread::spawn(move || unsafe { co.cancel() })
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
        unsafe { j.coroutine().cancel() };
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
        unsafe { j.coroutine().cancel() };
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
        unsafe { co.cancel() };
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
        unsafe { co.cancel() };
        unsafe { co.cancel() };
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
        unsafe { j.coroutine().cancel() };
        running.store(false, Ordering::Release);
        assert!(j.join().is_err_and(|e| is_cancel(&*e)));
    }
}

/// Cancel a coroutine while it is parked on a `Mutex` with cancellation
/// *disabled* — the state `Condvar::wait_impl` enters for its internal
/// re-lock — must not corrupt the mutex's waiter count.
///
/// The single cancel wakes the coroutine from the condvar wait with
/// `Canceled`. It then re-locks the mutex with the cancel bit disabled, so
/// that re-lock keeps waiting instead of panicking at the yield point: the
/// coroutine only leaves once the mutex is handed over, and then releases the
/// lock and propagates the cancel. This is the deferred-cancel (`b_ignore`)
/// path, and it is fully exercised by one cancel — a second one arrives
/// already canceled and is not needed.
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

    // hold the mutex so the coroutine's re-lock has to park
    let mut guard = pair.0.lock().unwrap();
    *guard += 1;

    // the cancel wakes the condvar wait; the re-lock that follows runs with
    // cancel disabled, so it parks on the mutex instead of panicking
    unsafe { h.coroutine().cancel() };
    thread::sleep(Duration::from_millis(200));

    // hand the mutex over to the coroutine
    drop(guard);
    assert!(h.join().is_err_and(|e| is_cancel(&*e)));

    // the mutex must be free and usable afterwards, with its data intact
    let g = pair
        .0
        .try_lock()
        .expect("mutex corrupted after cancel: try_lock failed on a free mutex");
    assert_eq!(*g, 1, "mutex data corrupted after cancel");
    drop(g);
}

/// A cancel delivered while the coroutine is parked inside `Condvar`'s
/// internal mutex re-lock, where cancellation is disabled, must not corrupt
/// the mutex either.
///
/// That re-lock runs with the cancel bit disabled so it panics at a safe
/// point instead of inside `wait_impl`; it therefore has to keep waiting for
/// the hand-off, and the mutex must be left free and unlocked afterwards.
///
/// The second `cancel()` delivers the late cancel deterministically. A single
/// `cancel()` can land in the same window whenever the coroutine has already
/// moved from the condvar wait into the re-lock, so this is a real
/// interleaving and not just a repeated-cancel corner case.
#[test]
fn late_cancel_during_condvar_relock_keeps_mutex_consistent() {
    let pair = Arc::new((Mutex::new(0usize), Condvar::new()));

    let p = pair.clone();
    let h = go!(move || {
        let (m, c) = &*p;
        let g = m.lock().unwrap();
        let _ = c.wait(g);
        unreachable!("wait() returned after cancel");
    });

    // let the coroutine reach the condvar wait
    thread::sleep(Duration::from_millis(100));

    // hold the mutex so the re-lock that follows the cancel has to park
    let mut guard = pair.0.lock().unwrap();
    *guard += 1;

    // first cancel: wakes the coroutine from the condvar wait
    unsafe { h.coroutine().cancel() };
    thread::sleep(Duration::from_millis(200));

    // second cancel: lands while the re-lock is parked with cancel disabled
    unsafe { h.coroutine().cancel() };
    thread::sleep(Duration::from_millis(200));

    drop(guard);
    assert!(h.join().is_err_and(|e| is_cancel(&*e)));

    let g = pair
        .0
        .try_lock()
        .expect("mutex corrupted by a late cancel: try_lock failed on a free mutex");
    assert_eq!(*g, 1, "mutex data corrupted by a late cancel");
    drop(g);
}

/// A cancel that loses the race with a hand-off cannot wake the coroutine:
/// `Cancel::cancel` needs to take the parked coroutine out of the parker, and
/// the unlock already got there. The cancel bit is left set while the park
/// returns `Ok`, so the lock wait has to re-check it. The guard may not be
/// handed to an already-canceled wait, or the cancel is silently lost until
/// the coroutine's next cancellation point, or forever.
///
/// The holder runs as a coroutine, runs the waiter inline with `spawn_local`
/// until it parks, then issues the cancel immediately after the hand-off
/// without yielding in between. The wake schedules the waiter on the holder's
/// local run queue (see `Scheduler::schedule`), so the waiter cannot resume
/// until after the cancel bit is set. Racing the hand-off from another OS
/// thread instead lets the waiter resume between the two, pass its post-park
/// check, and acquire the lock; that is a legitimate late cancel (the lock was
/// granted first), not a lost one, and `order` misreports it as a failure when
/// the waiter is descheduled before recording its acquisition.
#[test]
fn cancel_racing_handoff_is_not_lost_on_mutex_lock() {
    for _ in 0..10 {
        let m = Arc::new(Mutex::new(0usize));
        let order = Arc::new(AtomicUsize::new(0));
        // position of the coroutine's acquisition in `order`, if it got that
        // far
        let acquired_at = Arc::new(AtomicUsize::new(usize::MAX));

        let (p, s, a) = (m.clone(), order.clone(), acquired_at.clone());
        let holder = go!(move || {
            // hold the lock so the waiter has to park on it
            let guard = p.lock().unwrap();

            // Run the waiter inline until it parks on the lock. The lock is
            // still held, so the wait must park, and `spawn_local` returns
            // only after that: the "waiter is parked" instant needs no timing
            // assumption. The holder also never leaves its worker, so the
            // hand-off below is queued on this worker's local run queue and
            // the waiter cannot run until the cancel bit is set.
            let (wp, ws, wa) = (p.clone(), s.clone(), a.clone());
            let waiter = unsafe {
                coroutine::Builder::new().spawn_local(move || {
                    let mut g = wp.lock().unwrap();
                    // a canceled wait has to panic inside `lock()`, not get
                    // here
                    wa.store(ws.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
                    *g += 1;
                })
            }
            .unwrap();

            // the unlock hands the lock over and consumes the park
            // registration, so this cancel has nothing left to wake and only
            // sets the bit
            drop(guard);
            unsafe { waiter.coroutine().cancel() };
            let cancel_at = s.fetch_add(1, Ordering::SeqCst);

            let _ = waiter.join();
            let acq = a.load(Ordering::SeqCst);
            assert!(
                acq == usize::MAX || acq < cancel_at,
                "canceled wait was handed the lock: acquired={acq} cancel={cancel_at}"
            );
            // the body only runs for the legitimate late-cancel ordering, and
            // the mutex must be usable and consistent afterwards either way
            let expected = if acq == usize::MAX { 0 } else { 1 };
            assert_eq!(
                *p.lock().unwrap(),
                expected,
                "mutex inconsistent after cancel"
            );
        });
        holder.join().unwrap();
    }
}

/// Same as [`cancel_racing_handoff_is_not_lost_on_mutex_lock`], for the write
/// side of an `RwLock`: the holder coroutine parks the waiter inline and then
/// performs the hand-off and the cancel back to back, so the woken waiter
/// cannot pass its post-park check in between.
#[test]
fn cancel_racing_handoff_is_not_lost_on_rwlock_write() {
    for _ in 0..10 {
        let m = Arc::new(RwLock::new(0usize));
        let order = Arc::new(AtomicUsize::new(0));
        let acquired_at = Arc::new(AtomicUsize::new(usize::MAX));

        let (p, s, a) = (m.clone(), order.clone(), acquired_at.clone());
        let holder = go!(move || {
            // hold the write lock so the waiter has to park on it
            let guard = p.write().unwrap();

            // Run the waiter inline until it parks on the lock: `spawn_local`
            // returns only once the wait is parked, so no timing assumption is
            // needed, and the holder stays on its worker for the hand-off.
            let (wp, ws, wa) = (p.clone(), s.clone(), a.clone());
            let waiter = unsafe {
                coroutine::Builder::new().spawn_local(move || {
                    let mut g = wp.write().unwrap();
                    // a canceled wait has to panic inside `write()`, not get
                    // here
                    wa.store(ws.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
                    *g += 1;
                })
            }
            .unwrap();

            // the unlock hands the lock over and consumes the park
            // registration, so this cancel has nothing left to wake and only
            // sets the bit
            drop(guard);
            unsafe { waiter.coroutine().cancel() };
            let cancel_at = s.fetch_add(1, Ordering::SeqCst);

            let _ = waiter.join();
            let acq = a.load(Ordering::SeqCst);
            assert!(
                acq == usize::MAX || acq < cancel_at,
                "canceled wait was handed the write lock: acquired={acq} cancel={cancel_at}"
            );
            // the body only runs for the legitimate late-cancel ordering, and
            // the rwlock must be usable and consistent afterwards either way
            let expected = if acq == usize::MAX { 0 } else { 1 };
            assert_eq!(
                *p.write().unwrap(),
                expected,
                "rwlock inconsistent after cancel"
            );
        });
        holder.join().unwrap();
    }
}

/// The condvar twin of [`cancel_racing_handoff_is_not_lost_on_mutex_lock`]: a
/// `notify_one` that beats `cancel()` consumes the park registration, so the
/// cancel only sets the bit and the wait resumes with `Ok`.
/// `Condvar::wait` must then take the cancel path — release the guard and
/// panic — instead of handing the guard back to a canceled wait.
#[test]
fn cancel_racing_notify_is_not_lost_on_condvar_wait() {
    for _ in 0..10 {
        let pair = Arc::new((Mutex::new(0usize), Condvar::new()));
        let ready = Arc::new(AtomicBool::new(false));
        let order = Arc::new(AtomicUsize::new(0));
        // position of the wait's return in `order`, if it returned at all
        let returned_at = Arc::new(AtomicUsize::new(usize::MAX));

        let p = pair.clone();
        let (r, s, ret) = (ready.clone(), order.clone(), returned_at.clone());
        let h = go!(move || {
            let (m, c) = &*p;
            let g = m.lock().unwrap();
            r.store(true, Ordering::SeqCst);
            let g = c.wait(g).unwrap();
            // a canceled wait has to panic inside `wait`, not get here
            ret.store(s.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
            drop(g);
        });

        while !ready.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
        // let the coroutine reach the condvar wait
        thread::sleep(Duration::from_millis(50));

        // Hold the mutex across the notify and the cancel. The notify delivers
        // the signal and consumes the park registration, so the cancel has
        // nothing left to wake and only sets the bit. Holding the mutex keeps
        // the woken waiter from finishing its re-lock until the cancel is
        // issued, which guarantees the waiter observes the bit before it can
        // return the guard. Without this, a waiter that checks just before the
        // bit is set may return first and only be descheduled before writing
        // `returned_at`, which would make the marker land after `cancel_at`
        // even though the wait was not canceled.
        let hold = pair.0.lock().unwrap();
        pair.1.notify_one();
        unsafe { h.coroutine().cancel() };
        let cancel_at = order.fetch_add(1, Ordering::SeqCst);
        drop(hold);

        let _ = h.join();
        let ret = returned_at.load(Ordering::SeqCst);
        assert!(
            ret == usize::MAX || ret < cancel_at,
            "canceled wait returned the guard: returned={ret} cancel={cancel_at}"
        );
        // the wait released the mutex on its way out
        let g = pair
            .0
            .try_lock()
            .expect("mutex corrupted by a canceled condvar wait");
        assert_eq!(*g, 0, "mutex data corrupted by a canceled condvar wait");
        drop(g);
    }
}
