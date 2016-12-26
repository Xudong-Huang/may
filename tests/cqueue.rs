#[macro_use]
extern crate coroutine;
pub use coroutine::cqueue;
pub use coroutine::cqueue::PollError::*;


#[test]
fn cqueue_drop() {
    let cqueue = cqueue::Cqueue::new();
    let i = 2;

    for m in 1..10 {
        let tocken = m;
        cqueue.add(tocken, move |mut es| {
            let j = i + tocken;
            println!("t={}", tocken);
            es.extra = j;
            es.send();
            println!("j={}", j)
        });
    }

    // the selectors run
    std::thread::sleep(std::time::Duration::from_millis(100));
    std::mem::drop(cqueue);

    println!("cqueue finished");
}

#[test]
fn cqueue_poll() {
    let cqueue = cqueue::Cqueue::new();
    let i = 2;

    for m in 1..10 {
        let tocken = m;
        cqueue.add(tocken, move |mut es| {
            let j = i + tocken;
            println!("t={}", tocken);
            es.extra = j;
            es.send();
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

    println!("cqueue finished");
}
