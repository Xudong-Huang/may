#![feature(libc)]

#[macro_use]
extern crate coroutine;
extern crate generator;
extern crate libc;

use libc::rand;
use std::time::Duration;

use coroutine::cqueue;

// sum ten data resources
fn main() {
    let mut gv = vec![];
    let mut total = 0;

    // create the event producers
    for i in 0..4 {
        let g = generator::Gn::new_scoped(move |mut s| {
            loop {
                coroutine::sleep(Duration::from_millis(500 * (i + 1)));
                let data = unsafe { rand() };
                // println!("coroutine{}: data = {:?}", i, data);
                s.yield_with(data >> 8);
            }
        });

        gv.push(g);
    }


    // the select body that monitor the rx event and recalc the new total
    cqueue::scope(|cqueue| {
        // registe select coroutines
        for t in 0..4 {
            cqueue.add(t, |es| {
                let mut last = 0;
                let tocken = es.get_tocken();
                while let Some(data) = gv[tocken].next() {
                    let delta = data - last;

                    // =====================================================
                    // send out our delta
                    es.send(&delta as *const _ as _);
                    // =====================================================

                    // bottom half that will run sequencially in the poller
                    println!("in selector: update from {}, data={}, last_total={}",
                                     tocken, data, total);
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
            println!("in poller: delta={}, total={}", delta, total);
        }
    });

    println!("done");
}
