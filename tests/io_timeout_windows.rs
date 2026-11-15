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
//! unit test in `src/io/sys/windows/iocp.rs`; this file checks the end-to-end
//! behavior through the event loop and the public adapters.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream as StdTcpStream};
use std::os::windows::io::{AsRawSocket, FromRawSocket, IntoRawSocket};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use may::net::TcpStream;

const WORKERS: usize = 8;
const LONG_TIMEOUT: Duration = Duration::from_secs(30);
const SHORT_TIMEOUT: Duration = Duration::from_secs(1);

/// All tests share the one leaked scheduler, so run them one at a time; every
/// test initializes it with the same worker count.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poison| poison.into_inner())
}

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
    let _guard = serial();
    // The scheduler is initialized once with the worker count the tests need.
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

/// Fill a listener's accept queue so that further connects stay pending.
///
/// Windows drops the SYN of a connection that finds the queue full, so a
/// local connect then hangs until it is canceled instead of failing fast.
fn fill_accept_queue(listener: &TcpListener) -> Vec<StdTcpStream> {
    let addr = listener.local_addr().unwrap();
    let mut held = Vec::new();
    let mut consecutive_timeouts = 0;
    for _ in 0..1024 {
        match StdTcpStream::connect_timeout(&addr, Duration::from_millis(50)) {
            Ok(s) => {
                held.push(s);
                consecutive_timeouts = 0;
            }
            Err(e) if e.kind() == ErrorKind::TimedOut => {
                consecutive_timeouts += 1;
                // A single timeout can be a transient stall; three in a row
                // mean the accept queue is really full.
                if consecutive_timeouts >= 3 {
                    break;
                }
            }
            Err(e) => panic!("filling the accept queue failed: {e:?}"),
        }
    }
    assert!(
        consecutive_timeouts >= 3 && held.len() >= 64,
        "could not fill the accept queue ({} connections)",
        held.len()
    );
    held
}

/// `connect_timeout` must cancel a ConnectEx that stays pending.
#[test]
fn connect_timeout_returns_timed_out() {
    let _guard = serial();
    may::config().set_workers(WORKERS);

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let held = fill_accept_queue(&listener);

    let (tx, rx) = mpsc::channel();
    spawn_on(0, move || {
        let _ = tx.send(may::net::TcpStream::connect_timeout(
            &addr,
            Duration::from_millis(100),
        ));
    });

    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("connect_timeout did not complete");
    let err = result.expect_err("connecting to a full accept queue should time out");
    assert_eq!(err.kind(), ErrorKind::TimedOut);

    drop(held);
    drop(listener);
}

/// A timed read that receives data before the deadline must return the data,
/// with the timeout disarmed rather than aborting the read.
#[test]
fn timed_read_returns_data_before_the_timeout() {
    let _guard = serial();
    may::config().set_workers(WORKERS);

    let Pair { stream, peer, .. } = Pair::new();
    let writer = std::thread::spawn(move || {
        let mut peer = peer;
        std::thread::sleep(Duration::from_millis(20));
        peer.write_all(&[42u8]).unwrap();
        // Keep the connection open until the reader has consumed the byte.
        std::thread::sleep(Duration::from_millis(100));
    });

    let (tx, rx) = mpsc::channel();
    spawn_on(0, move || {
        let mut stream = stream;
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut buf = [0u8; 1];
        let _ = tx.send(stream.read(&mut buf).map(|n| (n, buf[0])));
    });

    let (n, byte) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the timed read did not complete")
        .expect("the read should return the data");
    assert_eq!((n, byte), (1, 42));
    writer.join().unwrap();
}
