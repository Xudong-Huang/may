extern crate may;

use may::coroutine;
use coroutine::yield_now;

fn main() {
    let mut array = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    println!("old array: {:?}", array);
    coroutine::scope(|scope| {
        for i in &mut array {
            scope.spawn(move || {
                coroutine::scope(|scope| {
                    scope.spawn(|| {
                        println!("another scope get as {:?}", *i);
                        yield_now();
                        *i += 1;
                        println!("another scope set to {:?}", *i);
                    });
                });
                println!("get element as {:?}", *i);
                yield_now();
                *i += 1;
                println!("set element to {:?}", *i);
            });
        }
    });

    println!("new array: {:?}", array);
}
