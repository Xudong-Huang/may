use std::mem;
use generator::{get_context, yield_with};
use super::{Coroutine, EventSource, EventSubscriber, Scheduler};

pub struct Yield {
    sched: *mut Scheduler,
}

impl EventSource for Yield {
    fn subscribe(&mut self, co: Coroutine) {
        // just repush the coroutine to the ready list
        let sched = unsafe { &mut *self.sched };
        sched.schedule(co);
        // println!("yield_out()");
    }
}

#[inline]
pub fn _yield_with(es: EventSubscriber) {
    yield_with::<EventSubscriber>(es);
}

#[inline]
pub fn yield_out() {
    let context = get_context();
    let mut y = Yield { sched: context.extra as *mut _ };
    // it's safe to use the stack value here
    let r = unsafe { mem::transmute(&mut y as &mut EventSource) };
    let es = EventSubscriber { resource: r };
    _yield_with(es);
}
