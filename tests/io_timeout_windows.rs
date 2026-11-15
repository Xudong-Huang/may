#![cfg(all(windows, feature = "io_timeout"))]

//! Windows io-timeout regression tests.
//!
//! With more than one worker, every worker owns a selector with its own io
//! timer list. An io timer must be armed on the selector of the socket's
//! worker, and that worker must be woken when an earlier timer is added.
//! Otherwise the timeout sits behind whatever far-away deadline the shard is
//! already parked on, and a short read timeout can overshoot by the whole
//! long timeout.
//!
//! The exact arm/wakeup shard arithmetic is asserted deterministically by the
//! unit test in `src/io/sys/windows/iocp.rs`; this test checks the end-to-end
//! consequence through the event loop: a shard parked on a 30s timer must
//! still serve a newly armed 1s timer.

use std::io::{ErrorKind, Read};
use std::net::{TcpListener, TcpStream as StdTcpStream};
use std::os::windows::io::{AsRawSocket, FromRawSocket, IntoRawSocket};
use std::sync::mpsc;
use std::time::Duration;

use may::net::TcpStream;

const WORKERS: usize = 8;
const LONG_TIMEOUT: Duration = Duration::from_secs(30);
const SHORT_TIMEOUT: Duration = Duration::from_secs(1);

struct Pair {
    stream: TcpStream,
    peer: StdTcpStream,
    raw: usize,
}

impl Pair {
    /// A connected pair: the may side is registered with the io reactor, while
    /// the blocking side is kept alive by the caller so no EOF arrives early.
    fn new() -> Pair {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let peer = StdTcpStream::connect(addr).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let raw = stream.as_raw_socket() as usize;
        let stream = unsafe { TcpStream::from_raw_socket(stream.into_raw_socket()) };
        Pair { stream, peer, raw }
    }
}

/// Spawn on a specific worker, so the scheduler's own wakeup packet goes to a
/// worker of our choosing instead of one that could mask the io-timer wakeup.
fn spawn_on(id: usize, f: impl FnOnce() + Send + 'static) {
    unsafe {
        may::coroutine::Builder::new().id(id).spawn(f).unwrap();
    }
}

/// The shard `add_socket` assigns a handle to.
fn socket_shard(raw: usize) -> usize {
    (raw >> 2) % WORKERS
}

/// The shard the old io-timer code assigned a handle to.
fn old_timer_shard(raw: usize) -> usize {
    (raw % WORKERS) >> 2
}

/// A short read timeout must fire even while other io timers keep its
/// selector shard parked on a much later deadline.
#[test]
fn short_io_timeout_is_not_starved_by_a_later_one_on_its_shard() {
    // This integration binary has a single test, so the scheduler starts with
    // the worker count the test needs.
    may::config().set_workers(WORKERS);

    // Create the sockets up front, then pick two that share a non-zero
    // selector shard. A pair whose handles also share the old timer formula's
    // shard is preferred: on the old code that reproduces the bug in both the
    // timer's shard and the wakeup target.
    let mut pool: Vec<Pair> = (0..64).map(|_| Pair::new()).collect();

    let preferred = (0..pool.len()).find_map(|i| {
        (i + 1..pool.len()).find_map(|j| {
            let shard = socket_shard(pool[i].raw);
            let old = old_timer_shard(pool[i].raw);
            (shard != 0
                && shard == socket_shard(pool[j].raw)
                && old != 0
                && old == old_timer_shard(pool[j].raw))
            .then_some((i, j))
        })
    });

    let (i, j) = preferred
        .or_else(|| {
            (0..pool.len()).find_map(|i| {
                (i + 1..pool.len()).find_map(|j| {
                    let shard = socket_shard(pool[i].raw);
                    (shard != 0 && shard == socket_shard(pool[j].raw)).then_some((i, j))
                })
            })
        })
        .expect("could not find two sockets on the same non-zero shard");

    let Pair {
        stream: mut long,
        peer: long_peer,
        ..
    } = pool.swap_remove(j);
    let Pair {
        stream: mut short,
        peer: short_peer,
        ..
    } = pool.swap_remove(i);
    // The remaining candidates just close their connections.
    drop(pool);

    spawn_on(0, move || {
        long.set_read_timeout(Some(LONG_TIMEOUT)).unwrap();
        let mut buf = [0u8; 1];
        let _ = long.read(&mut buf);
    });

    // Let the long read reach its selector and arm its timer.
    std::thread::sleep(Duration::from_millis(100));

    // Force every shard through a `schedule_timer` round so each of them
    // recomputes its next wait from the long timer. Without this a worker that
    // has never seen an event is still in its first, indefinite select and
    // would pick the short timer up on the next spurious wakeup.
    for id in 0..WORKERS {
        spawn_on(id, || ());
    }
    std::thread::sleep(Duration::from_millis(100));

    let (tx, rx) = mpsc::channel();
    // Spawn on worker 0, which is not the timer's shard (both the socket and
    // the old timer mapping are non-zero for the chosen pair), so this
    // coroutine's own scheduler wakeup cannot mask the io-timer wakeup.
    spawn_on(0, move || {
        short.set_read_timeout(Some(SHORT_TIMEOUT)).unwrap();
        let mut buf = [0u8; 1];
        let _ = tx.send(short.read(&mut buf));
    });

    let result = rx.recv_timeout(Duration::from_secs(6)).unwrap_or_else(|e| {
        panic!(
            "a {SHORT_TIMEOUT:?} read did not finish within 6s while its shard \
             was parked on a {LONG_TIMEOUT:?} timer: {e:?}"
        )
    });
    let err = result.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::TimedOut);

    // Closing the peers releases both reads: the short one is already done,
    // and the long one must not have to wait for its 30s timeout.
    drop(short_peer);
    drop(long_peer);
}
