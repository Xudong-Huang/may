//! Timed waits in a coroutine must return for any timeout, including ones
//! shorter than the timer's resolution.

use may::sync::Semphore;
use std::sync::mpsc;
use std::time::Duration;

fn returns_within(d: Duration, n: usize) -> bool {
    let (tx, rx) = mpsc::channel();
    may::go!(move || {
        let s = Semphore::new(0);
        for _ in 0..n {
            assert!(!s.wait_timeout(d));
        }
        tx.send(()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(10)).is_ok()
}

#[test]
fn semaphore_wait_timeout_returns_for_short_timeouts() {
    for d in [
        Duration::ZERO,
        Duration::from_micros(1),
        Duration::from_micros(100),
        Duration::from_micros(500),
        Duration::from_millis(1),
        Duration::from_millis(2),
    ] {
        assert!(returns_within(d, 200), "wait_timeout({d:?}) did not return");
    }
}

#[test]
fn park_timeout_returns_for_short_timeouts() {
    let (tx, rx) = mpsc::channel();
    may::go!(move || {
        for d in [
            Duration::ZERO,
            Duration::from_micros(50),
            Duration::from_millis(1),
        ] {
            for _ in 0..200 {
                may::coroutine::park_timeout(d);
            }
        }
        tx.send(()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_secs(10)).is_ok(),
        "park_timeout did not return"
    );
}
