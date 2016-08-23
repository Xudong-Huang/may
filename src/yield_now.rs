use generator::yield_with as gen_yield;
use coroutine::{Coroutine, EventSource, EventSubscriber};
use scheduler::get_scheduler;

struct Yield {
}

impl EventSource for Yield {
    fn subscribe(&mut self, co: Coroutine) {
        // just repush the coroutine to the ready list
        get_scheduler().schedule(co);
        // println!("yield_out()");
    }
}

/// yield internal `EventSource` ref
/// it's ok to return a ref of object on the generator's stack
/// just like return the ref of a struct member
#[inline]
pub fn yield_with<T: EventSource>(resource: &T) {
    let r = resource as &EventSource as *const _ as *mut _;
    let es = EventSubscriber::new(r);
    gen_yield(es);
}

#[inline]
pub fn yield_now() {
    let y = Yield {};
    // it's safe to use the stack value here
    yield_with(&y);
}
