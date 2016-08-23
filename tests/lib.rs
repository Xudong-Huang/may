#![feature(test)]
use std::thread;
use std::time::Duration;
extern crate test;
extern crate coroutine;

use coroutine::yield_now;

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
