//! Timed waits in a coroutine must return for any timeout, including ones
//! shorter than the timer's resolution.

use may::sync::Semaphore;
use std::sync::mpsc;
use std::time::Duration;

fn returns_within(d: Duration, n: usize) -> bool {
    let (tx, rx) = mpsc::channel();
    may::go!(move || {
        let s = Semaphore::new(0);
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

#[cfg(unix)]
#[test]
fn large_timeout_does_not_panic() {
    // Test that very large timeouts don't cause panics in the epoll/kqueue selector.
    // This is a regression test for an issue where EpollTimeout::try_from would panic
    // on values > i32::MAX milliseconds (e.g., 30 days = 2,592,000,000 ms).
    let (tx, rx) = mpsc::channel();
    let co = unsafe {
        may::coroutine::Builder::new()
            .spawn_local(move || {
                // Very large timeout (30 days)
                let large_timeout = Duration::from_secs(30 * 24 * 3600);
                // Using a short timeout after the large one to ensure the test doesn't hang
                may::coroutine::park_timeout(large_timeout);
                println!("Large timeout completed by outside unpark");
                may::coroutine::park_timeout(Duration::from_millis(1));
                tx.send(()).unwrap();
            })
            .unwrap()
    };
    // don't wait that long for the large timeout, we will unpark it immediately
    co.coroutine().unpark();
    // Give it time to complete (should be quick since we don't actually wait 30 days)
    assert!(
        rx.recv().is_ok(),
        "park_timeout with large timeout did not complete"
    );
}
