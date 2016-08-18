use std::mem;
use generator::yield_with;
use coroutine::{Coroutine, EventSource, EventSubscriber};
use scheduler::get_scheduler;

pub struct Yield {
}

impl EventSource for Yield {
    fn subscribe(&mut self, co: Coroutine) {
        // just repush the coroutine to the ready list
        get_scheduler().schedule(co);
        // println!("yield_out()");
    }
}

#[inline]
pub fn _yield_with(es: EventSubscriber) {
    yield_with::<EventSubscriber>(es);
}

#[inline]
pub fn yield_now() {
    let mut y = Yield {};
    // it's safe to use the stack value here
    let r = unsafe { mem::transmute(&mut y as &mut EventSource) };
    let es = EventSubscriber::new(r);
    _yield_with(es);
}
