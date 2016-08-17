extern crate coroutine;

use coroutine::yield_out;

fn main() {
    coroutine::spawn(move || {
        println!("hi, I'm parent");
        for i in 0..10 {
            coroutine::spawn(move || {
                println!("hi, I'm child{:?}", i);
                yield_out();
                println!("bye from child{:?}", i);
            });
        }
        yield_out();
        println!("bye from parent");
    });
    coroutine::sched_run();
}
