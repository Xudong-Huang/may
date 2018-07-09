extern crate generator;
#[macro_use]
extern crate may;

use std::time::Duration;

use may::{coroutine, cqueue};

// this is wrapper to work around the compile error
// we are safe to share the data in bottom half since we run them orderly
struct SyncCell<T>(T);
impl<T> SyncCell<T> {
    unsafe fn get(&self) -> &mut T {
        &mut *(&self.0 as *const _ as *mut T)
    }
}

// sum ten data resources
fn main() {
    may::config().set_workers(4);
    let mut gv = vec![];
    let total = SyncCell(0);

    // create the event producers
    for i in 0..10 {
        let g = generator::Gn::new_scoped(move |mut s| {
            let mut data = 10;
            loop {
                coroutine::sleep(Duration::from_millis(500 * (i + 1)));
                // println!("coroutine{}: data = {:?}", i, data);
                s.yield_with(data);
                data += 10;
            }
        });

        gv.push(g);
    }

    // the select body that monitor the rx event and recalc the new total
    cqueue::scope(|cqueue| {
        // registe select coroutines
        for t in 0..10 {
            go!(cqueue, t, |es| {
                let mut last = 0;
                let token = es.get_token();
                while let Some(data) = gv[token].next() {
                    // =====================================================
                    es.send(0);
                    // =====================================================

                    let delta = data - last;
                    let total = unsafe { total.get() };
                    // bottom half that will run sequencially in the poller
                    println!(
                        "in selector: update from {}, delta={}, last_total={}",
                        token, delta, total
                    );

                    *total += delta;
                    last = data;
                }
            });
        }

        // register timer for poller
        cqueue_add_oneshot!(cqueue, 9999, _ = coroutine::sleep(Duration::from_secs(10)) => {
            println!("poll time over!");
        });

        let total = unsafe { total.get() };
        // poll the event
        while let Ok(ev) = cqueue.poll(None) {
            if ev.token == 9999 {
                break;
            }
            // print the new total
            println!("in poller: total={}", total);
        }
    });

    println!("done");
}
