#[macro_use]
extern crate may;

use crate::cqueue::PollError::*;
use may::{coroutine, cqueue};

use std::time::Duration;

#[test]
fn cqueue_drop() {
    let v = (0..10).map(|x| x * x).collect::<Vec<usize>>();
    cqueue::scope(|cqueue| {
        for token in 0..10 {
            go!(cqueue, token, |es| {
                let token = es.get_token();
                let j = v[token];
                es.send(0);
                println!("j={j}")
            });
        }

        // let the selectors run for some time
        std::thread::sleep(Duration::from_millis(100));
    });

    println!("cqueue finished");
}

#[test]
fn cqueue_in_coroutine() {
    let v = (0..10).map(|x| x * x).collect::<Vec<usize>>();
    go!(move || {
        cqueue::scope(|cqueue| {
            for token in 0..10 {
                go!(cqueue, token, |es| {
                    let token = es.get_token();
                    let j = v[token];
                    es.send(0);
                    println!("j={j}")
                });
            }

            loop {
                match cqueue.poll(None) {
                    Ok(ev) => println!("ev = {ev:?}"),
                    Err(Finished) => break,
                    Err(Timeout) => unreachable!(),
                }
            }
        });
    })
    .join()
    .unwrap();
    println!("cqueue finished");
}

#[test]
#[should_panic]
fn cqueue_panic() {
    let v = (0..10).map(|x| x * x).collect::<Vec<usize>>();
    cqueue::scope(|cqueue| {
        for token in 0..10 {
            go!(cqueue, token, |es| {
                let token = es.get_token();
                let j = v[token];
                es.send(0);
                println!("j={j}")
            });
        }

        // let the selectors run for some time
        std::thread::sleep(Duration::from_millis(100));
        panic!("panic in cqueue scope")
    });

    println!("cqueue finished");
}

#[test]
#[should_panic]
fn cqueue_panic_in_select() {
    cqueue::scope(|cqueue| {
        go!(cqueue, 0, |_es| {
            panic!("panic in selector");
        });
    });

    println!("cqueue finished");
}

#[test]
fn cqueue_poll() {
    let v = (0..10).map(|x| x * x).collect::<Vec<usize>>();
    cqueue::scope(|cqueue| {
        for token in 0..10 {
            go!(cqueue, token, |es| {
                let token = es.get_token();
                let j = v[token];
                es.send(token + 100);
                println!("j={j}");
            });
        }

        loop {
            match cqueue.poll(None) {
                Ok(ev) => println!("ev = {ev:?}"),
                Err(Finished) => break,
                Err(Timeout) => unreachable!(),
            }
        }
    });
    println!("cqueue finished");
}

#[test]
fn cqueue_oneshot() {
    use may::sync::mpsc::channel;

    let (tx1, rx1) = channel();
    let (tx2, rx2) = channel();

    cqueue::scope(|cqueue| {
        // `Receiver` is `!Sync`; move each receiver into its coroutine.
        cqueue_add_oneshot!(cqueue, 0, _ = { let rx1 = rx1; rx1.recv() } => println!("rx1 received"));
        cqueue_add_oneshot!(cqueue, 1, a = { let rx2 = rx2; rx2.recv() } => println!("rx2 received, a={a:?}"));

        tx1.send("hello").unwrap();
        match cqueue.poll(None) {
            Ok(ev) => println!("ev = {ev:?}"),
            _ => unreachable!(),
        }

        tx2.send(100).unwrap();
        match cqueue.poll(None) {
            Ok(ev) => println!("ev = {ev:?}"),
            _ => unreachable!(),
        }

        match cqueue.poll(None) {
            Err(x) => assert_eq!(x, Finished),
            _ => unreachable!(),
        }
    });
}

#[test]
fn cqueue_select() {
    // This test borrows receivers into the select branches and reuses one
    // afterwards; `mpsc::Receiver` is `!Sync` (single-consumer), so use the
    // multi-consumer channel here.
    use may::sync::mpmc::channel;

    let (tx1, rx1) = channel();
    let (tx2, rx2) = channel();

    go!(move || {
        tx2.send("hello").unwrap();
        coroutine::sleep(Duration::from_millis(100));
        tx1.send(42).unwrap();
    });

    let id = select!(
        _ = coroutine::sleep(Duration::from_millis(1000)) => {},
        _ = rx1.recv() => println!("rx1 received"),
        a = rx2.recv() => println!("rx2 received, a={a:?}")
    );

    assert_eq!(id, 2);
    assert_eq!(rx1.recv(), Ok(42));
}

#[test]
fn cqueue_timeout() {
    cqueue::scope(|cqueue| {
        cqueue_add_oneshot!(cqueue, 0, _ = coroutine::sleep(Duration::from_secs(10)) => {});
        match cqueue.poll(Some(Duration::from_millis(10))) {
            Err(x) => assert_eq!(x, Timeout),
            _ => unreachable!(),
        }
    });
}

#[test]
fn cqueue_loop() {
    // The `cqueue_add!` closures run in a loop and borrow their receivers,
    // so use the multi-consumer channel (`mpsc::Receiver` is `!Sync`).
    use may::sync::mpmc::channel;

    let (tx1, rx1) = channel();
    let (tx2, rx2) = channel();
    let mut result = 0;

    go!(move || {
        tx1.send(10).unwrap();
        tx1.send(20).unwrap();
        tx1.send(30).unwrap();
        tx1.send(40).unwrap();
        tx1.send(50).unwrap();
        // finish the test
        coroutine::sleep(Duration::from_millis(100));
        tx2.send(()).unwrap();
    });

    cqueue::scope(|cqueue| {
        // when the sender is closed, receiver will never blocked
        // so need to check the receiver state
        cqueue_add!(cqueue, 0, a = rx1.recv() => {
            match a {
                Ok(v) => result = v,
                _ => return,
            }
        });

        cqueue_add!(cqueue, 100, b = rx2.recv() => {
            match b {
                Ok(_) => {},
                _ => return,
            }
        });

        loop {
            match cqueue.poll(None) {
                Ok(ev) => {
                    if ev.token == 100 {
                        println!("poll loop finished");
                        return;
                    }
                    println!("result={result}");
                }
                _ => unreachable!(),
            }
        }
    });

    assert_eq!(result, 50);
}

// A selector panic observed by `poll` must propagate without poisoning the
// cqueue: `Drop` still has to cancel and drain the other selectors.
#[test]
#[should_panic(expected = "panic in selector")]
fn cqueue_panic_seen_by_poll() {
    cqueue::scope(|cqueue| {
        go!(cqueue, 0, |_es| {
            panic!("panic in selector");
        });
        // stays alive while the panic above propagates through `poll`
        cqueue_add_oneshot!(cqueue, 1, _ = coroutine::sleep(Duration::from_secs(30)) => {});

        std::thread::sleep(Duration::from_millis(100));
        let _ = cqueue.poll(None);
    });
}

// A selector panic detected while the scope is already unwinding must be
// dropped, not re-raised (that would abort the process).
#[test]
#[should_panic(expected = "panic in scope")]
fn cqueue_selector_panic_while_unwinding() {
    cqueue::scope(|cqueue| {
        go!(cqueue, 0, |_es| {
            panic!("panic in selector");
        });
        cqueue_add_oneshot!(cqueue, 1, _ = coroutine::sleep(Duration::from_secs(30)) => {});

        // let the selector panic and queue its Done event, then unwind the
        // scope while the other selector is still alive
        std::thread::sleep(Duration::from_millis(100));
        panic!("panic in scope");
    });
}

#[test]
fn cqueue_zero_timeout() {
    cqueue::scope(|cqueue| {
        cqueue_add_oneshot!(cqueue, 0, _ = coroutine::sleep(Duration::from_secs(30)) => {});
        let start = std::time::Instant::now();
        match cqueue.poll(Some(Duration::ZERO)) {
            Err(Timeout) => {}
            _ => unreachable!(),
        }
        assert!(start.elapsed() < Duration::from_secs(1));
    });
}

// Concurrent `add` is not supported and is rejected in debug builds.
#[test]
#[cfg(debug_assertions)]
fn cqueue_concurrent_add_is_rejected_in_debug() {
    cqueue::scope(|cqueue| {
        cqueue_add_oneshot!(cqueue, 0, _ = coroutine::sleep(Duration::from_secs(30)) => {});
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            s.spawn(|| {
                // `poll` below owns the cqueue while it waits
                std::thread::sleep(Duration::from_millis(50));
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = cqueue.add(1, |_es| {});
                }));
                let msg = r.err().map(|e| {
                    e.downcast_ref::<String>()
                        .cloned()
                        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_default()
                });
                tx.send(msg).unwrap();
            });
            match cqueue.poll(Some(Duration::from_millis(200))) {
                Err(Timeout) => {}
                _ => unreachable!(),
            }
        });
        let msg = rx.recv().unwrap();
        assert!(
            msg.as_deref().is_some_and(|m| m.contains("not supported")),
            "unexpected panic: {msg:?}"
        );
    });
}
