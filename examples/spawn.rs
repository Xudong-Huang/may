extern crate coroutine;

use coroutine::yield_now;

fn main() {
    let co = coroutine::spawn(move || {
        println!("hi, I'm parent");
        for i in 0..10 {
            coroutine::spawn(move || {
                println!("hi, I'm child{:?}", i);
                yield_now();
                println!("bye from child{:?}", i);
            });
        }
        yield_now();
        println!("bye from parent");
    });
    co.join();
}
