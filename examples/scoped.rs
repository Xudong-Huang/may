extern crate coroutine;

use coroutine::yield_now;

fn main() {
    let mut array = [1, 2, 3];
    println!("old array: {:?}", array);
    coroutine::scope(|scope| {
        for i in &mut array {
            scope.spawn(move || {
                println!("get element to {:?}", *i);
                yield_now();
                *i += 1;
                println!("set element to {:?}", *i);
            });
        }
    });

    println!("new array: {:?}", array);
}
