use std::sync::Arc;
use std::cell::UnsafeCell;
use generator::{Gn, Generator};
use scheduler::get_scheduler;
use join::{Join, JoinHandler};

pub struct EventResult {
}

pub struct EventSubscriber {
    resource: *mut EventSource,
}

impl EventSubscriber {
    pub fn new(r: *mut EventSource) -> Self {
        EventSubscriber { resource: r }
    }

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

// coroutines are static generator, the para type is EventResult, the result type is EventSubscriber
pub type Coroutine = Generator<'static, EventResult, EventSubscriber>;

struct Done;

impl EventSource for Done {
    fn subscribe(&mut self, _co: Coroutine) {
        // assert!(co.is_done(), "unfinished coroutine detected");
        // just consume the coroutine
    }
}

pub fn spawn<F: FnOnce() + 'static>(f: F) -> JoinHandler {
    static DONE: Done = Done {};
    let done = &DONE as &EventSource as *const _ as *mut EventSource;
    // create a join resource, shared by waited coroutine and *this* coroutine
    let my_join = Arc::new(UnsafeCell::new(Join::new()));
    let their_join = my_join.clone();
    let co = Gn::new(move || {
        f();
        // trigger the JoinHandler
        let join = unsafe { &mut *my_join.get() };
        join.trigger();
        EventSubscriber { resource: done }
    });

    // put the coroutine to ready list
    get_scheduler().schedule(co);
    JoinHandler(their_join)
}
