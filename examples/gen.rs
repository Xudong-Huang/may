extern crate coroutine;
extern crate generator;

use coroutine::yield_now;
use generator::Gn;

fn main() {
    coroutine::scope(|scope| {
        scope.spawn(|| {
            let g = Gn::<()>::new_scoped(|mut scope| {
                let (mut a, mut b) = (0, 1);
                while b < 200 {
                    std::mem::swap(&mut a, &mut b);
                    b = a + b;
                    scope.yield_(b);
                }
                a + b
            });
            g.fold((), |_, i| {
                println!("got {:?}", i);
                // this is yield from the generator context!
                yield_now();
            });
        });
    });
}
