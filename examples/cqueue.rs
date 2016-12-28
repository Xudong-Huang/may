#![feature(libc)]

#[macro_use]
extern crate coroutine;
extern crate libc;

use libc::rand;
use std::time::Duration;

use coroutine::cqueue;
use coroutine::sync::mpsc::channel;

// sum ten data resources
fn main() {
    let mut rx_vec = vec![];
    let mut total = 0;

    // create the event producers
    for i in 0..10 {
        let (tx, rx) = channel();
        rx_vec.push(rx);

        coroutine::spawn(move || {
            loop {
                coroutine::sleep(Duration::from_millis(500 * (i + 1)));
                let data = unsafe { rand() };
                // println!("coroutine{}: data = {:?}", i, data);
                if let Err(_) = tx.send(data) {
                    // println!("coroutine{} finished", i);
                    break;
                }
            }
        });
    }

    // the select body that monitor the rx event and recalc the new total
    cqueue::scope(move |cqueue| {
        // registe select coroutines
        for t in 0..10 {
            cqueue.add(t, |es| {
                let mut last = 0;
                let tocken = es.get_tocken();
                loop {
                    // top half for receive data
                    let data = match rx_vec[tocken].recv() {
                        Ok(d) => d,
                        Err(_) => return,
                    };
                    let delta = data - last;

                    // =====================================================
                    // send out our delta
                    es.send(&delta as *const _ as _);
                    // =====================================================

                    // bottom half that will run sequencially in the poller
                    println!("update from {}, data={}, last_total={}", tocken, data, total);
                    last = data;
                }
            });
        }

        // register timer for poller
        cqueue_add_oneshot!(cqueue, 9999, _ = coroutine::sleep(Duration::from_secs(10)) => {
            println!("poll time over!");
        });

        // poll the event
        while let Ok(ev) = cqueue.poll(None) {
            if ev.tocken == 9999 {
                break;
            }
            // calc the new total
            let delta = unsafe { *(ev.extra as *const _) };
            total += delta;
            println!("delta={}, total={}", delta, total);
        }
    });

    println!("done");
}
