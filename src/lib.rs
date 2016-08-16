#![feature(test)]
extern crate test;
extern crate generator;
extern crate queue;

use std::mem;

use generator::{Gn, Generator, get_context, yield_with};

use queue::{BulkPop, /* McQueueExt, */ BLOCK_SIZE};
// use queue::mpmc::Queue as mpmc;
// use queue::mpsc::Queue as mpsc;
use queue::spsc::Queue as spsc;

pub struct EventResult {

}

pub struct EventSubscriber {
    resource: *mut EventSource,
}

impl EventSubscriber {
    pub fn subscribe(self, c: Coroutine) {
        let resource = unsafe { &mut *self.resource };
        resource.subscribe(c);
    }
}

pub trait EventSource {
    /// this will trigger an event and cause the registered coroutine push into the 'ready list'
    fn trigger(&mut self) {}
    /// register a coroutine waiting on the resource
    fn subscribe(&mut self, _c: Coroutine) {}
}

pub type Coroutine = Generator<'static, EventResult, EventSubscriber>;
// coroutines are static generator, the para type is EventResult, the result type is EventSubscriber
// pub struct Coroutine {
//     handler: Generator<'static, EventResult, EventSubscriber>,
// }
//
// impl Coroutine {
//     #[inline]
//     pub fn resume(&mut self) -> Option<EventSubscriber> {
//         self.handler.resume()
//     }
// }

#[inline]
pub fn yield_out() {
    pub struct Yield {
        sched: *mut Scheduler,
    }

    impl EventSource for Yield {
        fn subscribe(&mut self, co: Coroutine) {
            // just repush the coroutine to the ready list
            let sched = unsafe { &mut *self.sched };
            sched.schedule(co);
        }
    }

    let context = get_context();
    let mut y = Yield { sched: context.extra as *mut _ };
    // it's safe to use the stack value here
    let r = unsafe { mem::transmute(&mut y as &mut EventSource) };
    let es = EventSubscriber { resource: r };
    _yield_with(es);
}

#[inline]
pub fn _yield_with(es: EventSubscriber) {
    yield_with::<EventSubscriber>(es);
}

pub struct Scheduler {
    ready_list: spsc<Coroutine>,
}

impl Scheduler {
    pub fn new() -> Box<Self> {
        Box::new(Scheduler { ready_list: spsc::new() })
    }

    pub fn run_to_complete(&self) {
        loop {
            let mut vec = Vec::with_capacity(BLOCK_SIZE);
            // let mut vec = Vec::new();
            let size = self.ready_list.bulk_pop(&mut vec);
            if size == 0 {
                break;
            }/*
            vec.drain(0..size)
                .fold((), |_, mut coroutine| {
                    let event_subscriber = coroutine.resume().unwrap();
                    // the coroutine will sink into the subscriber's resouce
                    // basically it just register the coroutine somewhere within the 'resource'
                    // 'resource' is just any type that live within the coroutine's stack
                    event_subscriber.subscribe(coroutine);
                    println!("aaaaa {:?}", size);
                });*/
            for mut coroutine in vec {
                let event_subscriber = coroutine.resume().unwrap();
                // the coroutine will sink into the subscriber's resouce
                // basically it just register the coroutine somewhere within the 'resource'
                // 'resource' is just any type that live within the coroutine's stack
                // println!("{:?}", self.ready_list);
                event_subscriber.subscribe(coroutine);
            }
        }
    }

    // pub fn start(&self, c: Coroutine) {
    //     self.ready_list.push(c);
    //     self.run_to_complete();
    // }

    /// put the coroutine to ready list so that next time it can be scheduled
    #[inline]
    pub fn schedule(&self, co: Coroutine) {
        self.ready_list.push(co);
    }

    pub fn make_co<F: FnOnce() + 'static>(&self, f: F) -> Coroutine {
        struct Done;
        impl EventSource for Done {}

        static DONE: Done = Done {};
        let done = &DONE as &EventSource as *const _ as *mut EventSource;
        let mut g = Gn::new(move || {
            f();
            EventSubscriber { resource: done }
        });

        {
            // this will set the scheduler as the context extra data
            let context = g.get_context();
            context.extra = self as *const _ as *mut _;
        }

        g
    }

    pub fn spawn<F: FnOnce() + 'static>(&self, f: F) {
        let co = self.make_co(f);
        // put the coroutine to ready list
        self.schedule(co);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        for i in 0..1000 {
            sched.spawn(move || {
                println!("hi, coroutine{}", i);
                yield_out();
                println!("bye, coroutine{}", i);
            });
        }
        sched.run_to_complete();
    }

}
