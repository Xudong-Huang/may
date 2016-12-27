#[macro_use]
extern crate coroutine;
pub use coroutine::cqueue;
pub use coroutine::cqueue::PollError::*;

use std::time::Duration;

#[test]
fn cqueue_drop() {
    coroutine::scheduler_config().set_workers(4);
    let v = (0..10).map(|x| x * x).collect::<Vec<usize>>();
    cqueue::scope(|cqueue| {
        for tocken in 0..10 {
            cqueue.add(tocken, |es| {
                let tocken = es.get_tocken();
                let j = v[tocken];
                es.send(0);
                println!("j={}", j)
            });
        }

        // let the selectors run for some time
        std::thread::sleep(Duration::from_millis(100));
    });

    println!("cqueue finished");
}

#[test]
#[should_panic]
fn cqueue_panic() {
    coroutine::scheduler_config().set_workers(4);
    let v = (0..10).map(|x| x * x).collect::<Vec<usize>>();
    cqueue::scope(|cqueue| {
        for tocken in 0..10 {
            cqueue.add(tocken, |es| {
                let tocken = es.get_tocken();
                let j = v[tocken];
                es.send(0);
                println!("j={}", j)
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
        cqueue.add(0, |_es| {
            panic!("painc in selector");
        });
    });

    println!("cqueue finished");
}

#[test]
fn cqueue_poll() {
    let v = (0..10).map(|x| x * x).collect::<Vec<usize>>();
    cqueue::scope(|cqueue| {
        for tocken in 0..10 {
            cqueue.add(tocken, |es| {
                let tocken = es.get_tocken();
                let j = v[tocken];
                es.send(tocken + 100);
                println!("j={}", j)
            });
        }

        loop {
            match cqueue.poll(None) {
                Ok(ev) => println!("ev = {:?}", ev),
                Err(Finished) => break,
                Err(Timeout) => unreachable!(),
            }
        }
    });
    println!("cqueue finished");
}

#[test]
fn cqueue_oneshot() {
    use coroutine::sync::mpsc::channel;

    let (tx1, rx1) = channel();
    let (tx2, rx2) = channel();

    cqueue::scope(|cqueue| {
        cqueue_add_oneshot!(cqueue, 0, _ = rx1.recv() => println!("rx1 received"));
        cqueue_add_oneshot!(cqueue, 1, a = rx2.recv() => println!("rx2 received, a={:?}", a));

        tx1.send("hello").unwrap();
        match cqueue.poll(None) {
            Ok(ev) => println!("ev = {:?}", ev),
            _ => unreachable!(),
        }

        tx2.send(100).unwrap();
        match cqueue.poll(None) {
            Ok(ev) => println!("ev = {:?}", ev),
            _ => unreachable!(),
        }

        match cqueue.poll(None) {
            Err(x) => assert_eq!(x,Finished ),
            _ => unreachable!(),
        }
    });
}

#[test]
fn cqueue_select() {
    use coroutine::sync::mpsc::channel;

    let (tx1, rx1) = channel();
    let (tx2, rx2) = channel();

    coroutine::spawn(move || {
        tx2.send("hello").unwrap();
        coroutine::sleep(Duration::from_millis(100));
        tx1.send(42).unwrap();
    });


    let id = select!(
        _ = coroutine::sleep(Duration::from_millis(1000)) => {},
        _ = rx1.recv() => println!("rx1 received"),
        a = rx2.recv() => println!("rx2 received, a={:?}", a)
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
    use coroutine::sync::mpsc::channel;

    let (tx1, rx1) = channel();
    let (tx2, rx2) = channel();
    let mut result = 0;

    coroutine::spawn(move || {
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
                    if ev.tocken == 100 {
                        println!("poll loop finished");
                        return;
                    }
                    println!("result={}", result);
                }
                _ => unreachable!(),
            }
        }
    });

    assert_eq!(result, 50);
}
