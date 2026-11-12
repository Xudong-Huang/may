//! Scheduler-level behavior tests.
//!
//! The tests below drive the process-global runtime with one worker, so they
//! take `SERIAL` and run one at a time. Integration tests in one file share a
//! process, hence one leaked scheduler: the scheduler is initialized once and
//! `set_workers` is only read at initialization. Serializing keeps each test
//! from observing the other's coroutines on that single worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A worker id outside `0..workers` must be rejected, not silently reduced
/// modulo the worker count and run on an unrelated worker.
#[test]
fn builder_id_out_of_range_is_rejected() {
    let _guard = serial();
    may::config().set_workers(1);

    for id in [1, usize::MAX] {
        let result = unsafe { may::coroutine::Builder::new().id(id).spawn(|| ()) };
        let err = result
            .err()
            .unwrap_or_else(|| panic!("worker id {id} should not be accepted"));
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    // a valid id still schedules the coroutine
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = unsafe {
        may::coroutine::Builder::new()
            .id(0)
            .spawn(move || tx.send(()).unwrap())
            .unwrap()
    };
    rx.recv_timeout(Duration::from_secs(5))
        .expect("coroutine spawned with a valid id did not run");
    handle.join().unwrap();
}

/// A coroutine that keeps re-queueing itself locally must not keep its worker
/// out of `select`.
///
/// `yield_now` pushes the running coroutine back onto the worker's local run
/// queue, so a yield loop never lets that queue become empty. The drain used
/// to run until the queue was empty, which meant the worker never re-entered
/// `select` while such a coroutine was runnable. The io completions and
/// per-selector io timers of that worker were then never serviced; with one
/// worker that is a deadlock.
///
/// The coroutine spawned from the os thread below can only be discovered by
/// `select`: it lands on the global scheduler queue, and the worker collects
/// that queue from `select` (the work-stealing drain only calls
/// `collect_global` once its local queue is empty, which the spinner never
/// allows). So it runs only if the worker gets back to `select` while the
/// spinner keeps yielding.
#[test]
fn self_yielding_coroutine_does_not_starve_select() {
    let _guard = serial();

    // One worker: the spinner and the late coroutine share the same local
    // queue and selector, so no other worker can pick up the late coroutine.
    may::config().set_workers(1);

    let started = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));

    // a coroutine that never blocks on io, so it is always runnable and keeps
    // re-queueing itself onto the worker's local queue
    let spinning_started = started.clone();
    let spinning_stop = stop.clone();
    may::go!(move || {
        spinning_started.store(true, Ordering::Release);
        while !spinning_stop.load(Ordering::Acquire) {
            may::coroutine::yield_now();
        }
    });

    // wait until the spinner is running on the worker, i.e. the drain has
    // started and (without the budget) can never return
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "the spinning coroutine did not start"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    // spawn from outside the runtime: this goes to the global queue, and only
    // `select` can collect it while the spinner keeps the local queue busy
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let late_stop = stop.clone();
    may::go!(move || {
        late_stop.store(true, Ordering::Release);
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the self-yielding coroutine starved the worker's selector");
}

/// Bursts of external spawns must all run even though the scheduler coalesces
/// wakeups behind one in-flight selector wakeup per worker.
///
/// Only the first push to a sleeping worker pays the wakeup syscall; the rest
/// of the burst ride on it. The dangerous interleaving is a producer
/// coalescing onto a wakeup the worker has already consumed: the worker must
/// disarm the flag and drain its global queue before it blocks again, or the
/// coalesced push is stranded. A lost wakeup leaves one of the coroutines
/// below unrunnable, so the receive times out.
#[test]
fn burst_spawns_from_outside_are_all_run() {
    let _guard = serial();

    // One worker: every push from the os thread targets the same global
    // queue, which maximizes wakeup coalescing.
    may::config().set_workers(1);

    // A resident coroutine that keeps re-queueing itself locally, so the
    // work-stealing drain never finds the local queue empty. The global queue
    // is then only collected from the selector wakeup event and from the
    // event loop's disarm, i.e. exactly the coalescing-safe paths.
    let started = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let spin_started = started.clone();
    let spin_stop = stop.clone();
    may::go!(move || {
        spin_started.store(true, Ordering::Release);
        while !spin_stop.load(Ordering::Acquire) {
            may::coroutine::yield_now();
        }
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "the spinning coroutine did not start"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    const ROUNDS: usize = 200;
    const BURST: usize = 32;

    let (tx, rx) = std::sync::mpsc::channel();
    for round in 0..ROUNDS {
        for i in 0..BURST {
            let tx = tx.clone();
            may::go!(move || {
                let _ = tx.send((round, i));
            });
        }
    }
    drop(tx);

    // The pushes race with the worker draining earlier coroutines, so this
    // covers producers arriving both while the worker sleeps and while it is
    // already awake with a wakeup armed.
    let mut seen = std::collections::HashSet::with_capacity(ROUNDS * BURST);
    for _ in 0..ROUNDS * BURST {
        let item = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a coalesced wakeup was lost");
        assert!(seen.insert(item), "coroutine ran twice: {item:?}");
    }

    stop.store(true, Ordering::Release);
}

/// Work scheduled by the io-timeout handler must run before the worker parks.
///
/// With `io_timeout`, the per-selector timer list resumes the timed-out
/// coroutine inline on the worker that owns the fd. Before the fix, that ran
/// after the event loop had drained its local run queue, so anything the
/// resumed coroutine scheduled locally (here a `yield_now`) stayed queued
/// until the next selector wakeup. The other coroutine below keeps a long io
/// timer armed on the same selector, which makes "the next selector wakeup" a
/// 30s park instead of the default 10ms poll.
#[cfg(all(unix, feature = "io_timeout"))]
mod io_timeout {
    use super::*;

    use std::io::Read;
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    use std::sync::mpsc;

    use may::net::TcpStream;

    /// A connected pair with no data flowing; the peer on the right is
    /// returned so the caller can keep it open.
    fn tcp_pair() -> (std::net::TcpStream, std::net::TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    fn may_stream(s: std::net::TcpStream) -> TcpStream {
        unsafe { TcpStream::from_raw_fd(s.into_raw_fd()) }
    }

    #[test]
    fn timeout_handler_schedules_before_parking() {
        let _guard = super::serial();

        // One worker makes both sockets share the same selector: the long
        // timer then controls the wakeup of the very event loop that resumes
        // the timed-out coroutine, and there is no other worker to steal the
        // queued task.
        may::config().set_workers(1);

        let (short_client, _short_peer) = tcp_pair();
        let (long_client, _long_peer) = tcp_pair();
        let mut short = may_stream(short_client);
        let mut long = may_stream(long_client);

        // Arm a far-away io timer on the selector. Nothing ever arrives on
        // this socket, so the read can only end by its timeout (or by the peer
        // drop when the test returns).
        may::go!(move || {
            long.set_read_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            let mut buf = [0u8; 1];
            let _ = long.read(&mut buf);
        });

        // The timer is armed when the coroutine suspends in `read`. Give the
        // worker time to get there, so that the long timer is already
        // installed when the short one fires and decides the next selector
        // wakeup.
        std::thread::sleep(Duration::from_millis(50));

        let (done_tx, done_rx) = mpsc::channel();
        may::go!(move || {
            short
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut buf = [0u8; 1];
            let err = short.read(&mut buf).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);

            // The timeout handler resumed us inline; this yield goes to the
            // worker's local queue and must still run before the worker parks.
            may::coroutine::yield_now();
            // the receiver is gone when the test already failed; don't panic
            // the coroutine on top of the assertion failure
            let _ = done_tx.send(());
        });

        done_rx.recv_timeout(Duration::from_secs(5)).expect(
            "work scheduled by the io-timeout handler slept until the next selector wakeup",
        );
    }
}
