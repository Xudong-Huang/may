//! Work scheduled by the io-timeout handler must run before the worker parks.
//!
//! With `io_timeout`, the per-selector timer list resumes the timed-out
//! coroutine inline on the worker that owns the fd. Before the fix, that ran
//! after the event loop had drained its local run queue, so anything the
//! resumed coroutine scheduled locally (here a `yield_now`) stayed queued
//! until the next selector wakeup. The other coroutine below keeps a long io
//! timer armed on the same selector, which makes "the next selector wakeup" a
//! 30s park instead of the default 10ms poll.

#![cfg(all(unix, feature = "io_timeout"))]

use std::io::Read;
use std::os::unix::io::{FromRawFd, IntoRawFd};
use std::sync::mpsc;
use std::time::Duration;

use may::net::TcpStream;

/// A connected pair with no data flowing; the peer on the right is returned
/// so the caller can keep it open.
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
    // One worker makes both sockets share the same selector: the long timer
    // then controls the wakeup of the very event loop that resumes the
    // timed-out coroutine, and there is no other worker to steal the queued
    // task.
    may::config().set_workers(1);

    let (short_client, _short_peer) = tcp_pair();
    let (long_client, _long_peer) = tcp_pair();
    let mut short = may_stream(short_client);
    let mut long = may_stream(long_client);

    // Arm a far-away io timer on the selector. Nothing ever arrives on this
    // socket, so the read can only end by its timeout (or by the peer drop
    // when the test returns).
    may::go!(move || {
        long.set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut buf = [0u8; 1];
        let _ = long.read(&mut buf);
    });

    // The timer is armed when the coroutine suspends in `read`. Give the
    // worker time to get there, so that the long timer is already installed
    // when the short one fires and decides the next selector wakeup.
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
        // the receiver is gone when the test already failed; don't panic the
        // coroutine on top of the assertion failure
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("work scheduled by the io-timeout handler slept until the next selector wakeup");
}
