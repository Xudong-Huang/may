#![feature(test)]
use std::thread;
use std::time::Duration;
extern crate test;
extern crate coroutine;
extern crate generator;

use coroutine::yield_now;
use generator::Gn;

#[test]
fn one_coroutine() {
    let j = coroutine::spawn(move || {
        println!("hello, coroutine");
    });
    j.join();
}


#[test]
fn coroutine_result() {
    let j = coroutine::spawn(move || {
        println!("hello, coroutine");
        100
    });

    assert_eq!(j.join(), 100);
}

#[test]
fn multi_coroutine() {
    for i in 0..10 {
        coroutine::spawn(move || {
            println!("hi, coroutine{}", i);
        });
    }
    thread::park_timeout(Duration::from_millis(200));
}

#[test]
fn test_yield() {
    let j = coroutine::spawn(move || {
        println!("hello, coroutine");
        yield_now();
        println!("goodbye, coroutine");
    });
    j.join();
}

#[test]
fn multi_yield() {
    for i in 0..10 {
        coroutine::spawn(move || {
            println!("hi, coroutine{}", i);
            yield_now();
            println!("bye, coroutine{}", i);
        });
    }
    thread::park_timeout(Duration::from_millis(200));
}

#[test]
fn spawn_inside() {
    coroutine::spawn(move || {
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
    thread::park_timeout(Duration::from_millis(200));
}

#[test]
fn wait_join() {
    let j = coroutine::spawn(move || {
        println!("hi, I'm parent");
        let join = (0..10)
            .map(|i| {
                coroutine::spawn(move || {
                    println!("hi, I'm child{:?}", i);
                    yield_now();
                    println!("bye from child{:?}", i);
                })
            })
            .collect::<Vec<_>>();

        for j in join {
            j.join();
        }
        println!("bye from parent");
    });
    j.join();
}

#[test]
fn scoped_coroutine() {
    let mut array = [1, 2, 3];
    coroutine::scope(|scope| {
        for i in &mut array {
            scope.spawn(move || {
                *i += 1;
            });
        }
    });

    assert_eq!(array[0], 2);
    assert_eq!(array[1], 3);
    assert_eq!(array[2], 4);
}

#[test]
fn yield_from_gen() {
    let mut a = 0;
    coroutine::scope(|scope| {
        scope.spawn(|| {
            let g = Gn::<()>::new_scoped(|mut scope| {
                while a < 10 {
                    scope.yield_(a);
                    a += 1;
                }
                a
            });
            g.fold((), |_, i| {
                println!("got {:?}", i);
                // this is yield from the generator context!
                yield_now();
            });
        });
    });

    assert_eq!(a, 10);
}
