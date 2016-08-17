#![feature(test)]
extern crate test;
extern crate coroutine;

use coroutine::yield_out;

#[test]
fn one_coroutine() {
    coroutine::spawn(move || {
        println!("hello, coroutine");
    });
    coroutine::sched_run();
}

#[test]
fn multi_coroutine() {
    for i in 0..10 {
        coroutine::spawn(move || {
            println!("hi, coroutine{}", i);
        });
    }
    coroutine::sched_run();
}

#[test]
fn test_yield() {
    coroutine::spawn(move || {
        println!("hello, coroutine");
        yield_out();
        println!("goodbye, coroutine");
    });
    coroutine::sched_run();
}

#[test]
fn multi_yield() {
    for i in 0..10 {
        coroutine::spawn(move || {
            println!("hi, coroutine{}", i);
            yield_out();
            println!("bye, coroutine{}", i);
        });
    }
    coroutine::sched_run();
}

#[test]
fn spawn_inside() {
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
