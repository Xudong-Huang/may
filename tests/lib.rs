#![feature(test)]
extern crate test;
extern crate coroutine;

use coroutine::*;


#[test]
fn one_coroutine() {
    let sched = Scheduler::new();
    sched.spawn(move || {
        println!("hello, coroutine");
    });
    sched.run_to_complete();
}

#[test]
fn multi_coroutine() {
    let sched = Scheduler::new();
    for i in 0..10 {
        sched.spawn(move || {
            println!("hi, coroutine{}", i);
        });
    }
    sched.run_to_complete();
}

#[test]
fn test_yield() {
    let sched = Scheduler::new();
    sched.spawn(move || {
        println!("hello, coroutine");
        yield_out();
        println!("goodbye, coroutine");
    });
    sched.run_to_complete();
}

#[test]
fn multi_yield() {
    let sched = Scheduler::new();
    for i in 0..10 {
        sched.spawn(move || {
            println!("hi, coroutine{}", i);
            yield_out();
            println!("bye, coroutine{}", i);
        });
    }
    sched.run_to_complete();
}/*

#[test]
fn spawn_inside() {
    let sched = Scheduler::new();
    sched.spawn(move || {
        println!("hi, coroutine{}", i);
        yield_out();
        println!("bye, coroutine{}", i);
    });
    sched.run_to_complete();
}*/
