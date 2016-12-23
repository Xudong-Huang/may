use std::thread;
use generator::{co_yield_with, co_get_yield, Error};
use coroutine::{is_coroutine, get_cancel_data};
use coroutine::{CoroutineImpl, EventResult, EventSource, EventSubscriber};
use scheduler::get_scheduler;

struct Yield {
}

impl EventSource for Yield {
    fn subscribe(&mut self, co: CoroutineImpl) {
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
    let cancel = resource.get_cancel_data().unwrap_or_else(|| get_cancel_data());
    if cancel.is_canceled() {
        panic!(Error::Cancel);
    }

    let r = resource as &EventSource as *const _ as *mut _;
    let es = EventSubscriber::new(r);
    co_yield_with(es);

    // after return back we should re-check the panic and clear it
    if cancel.is_canceled() {
        panic!(Error::Cancel);
    }
    resource.get_cancel_data().map(|cancel| cancel.clear_io());
}

/// set the coroutine para that passed into it
#[inline]
pub fn set_co_para(co: &mut CoroutineImpl, v: EventResult) {
    co.set_para(v);
}

/// get the coroutine para from the coroutine context
#[inline]
pub fn get_co_para() -> Option<EventResult> {
    co_get_yield::<EventResult>()
}

#[inline]
pub fn yield_now() {
    if !is_coroutine() {
        return thread::yield_now();
    }
    let y = Yield {};
    // it's safe to use the stack value here
    yield_with(&y);
}
