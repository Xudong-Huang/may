use std::sync::Arc;
use std::cell::{UnsafeCell, RefCell};
use generator::{Gn, Generator};
use scheduler::get_scheduler;
use join::{Join, JoinHandle, make_join_handle};

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

pub fn spawn<F, T>(f: F) -> JoinHandle<T>
    where F: FnOnce() -> T + Send + 'static,
          T: Send + 'static
{
    static DONE: Done = Done {};
    let done = &DONE as &EventSource as *const _ as *mut EventSource;
    // create a join resource, shared by waited coroutine and *this* coroutine
    let join = Arc::new(UnsafeCell::new(Join::new()));
    let packet = Arc::new(RefCell::new(None));
    let their_join = join.clone();
    let their_packet = packet.clone();
    let co = Gn::new(move || {
        *their_packet.borrow_mut() = Some(f());
        // trigger the JoinHandler
        let join = unsafe { &mut *their_join.get() };
        join.trigger();
        EventSubscriber { resource: done }
    });

    // put the coroutine to ready list
    get_scheduler().schedule(co);
    make_join_handle(join, packet)
}
