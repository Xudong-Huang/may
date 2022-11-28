use crate::coroutine_impl::{current_cancel_data, is_coroutine};
use crate::coroutine_impl::{CoroutineImpl, EventResult, EventSource, EventSubscriber};
use crate::likely::{likely, unlikely};
use crate::scheduler::get_scheduler;

use generator::{co_get_yield, co_set_para, co_yield_with};

struct Yield {}

impl EventSource for Yield {
    fn subscribe(&mut self, co: CoroutineImpl) {
        // just re-push the coroutine to the ready list
        get_scheduler().schedule(co);
    }
}

/// yield internal `EventSource` ref
/// it's ok to return a ref of object on the generator's stack
/// just like return the ref of a struct member
#[inline]
pub fn yield_with<T: EventSource>(resource: &T) {
    let cancel = current_cancel_data();
    // if cancel detected in user space
    // no need to get into kernel any more
    if unlikely(cancel.is_canceled()) {
        co_set_para(::std::io::Error::new(
            ::std::io::ErrorKind::Other,
            "Canceled",
        ));
        return resource.yield_back(cancel);
    }

    let r = resource as &dyn EventSource as *const _ as *mut _;
    let es = EventSubscriber::new(r);
    co_yield_with(es);

    resource.yield_back(cancel);
    cancel.clear();
}

#[inline]
pub fn yield_with_io<T: EventSource>(resource: &T, is_coroutine: bool) {
    if likely(is_coroutine) {
        #[cfg(feature = "io_cancel")]
        yield_with(resource);
        #[cfg(not(feature = "io_cancel"))]
        {
            let r = resource as &dyn EventSource as *const _ as *mut _;
            let es = EventSubscriber::new(r);
            co_yield_with(es);
        }
    } else {
        // for thread is only park the thread
        let r = resource as &dyn EventSource as *const _ as *mut _;
        let es = EventSubscriber::new(r);
        crate::io::thread::PROXY_CO_SENDER.with(|tx| {
            tx.send(es).unwrap();
        });
        std::thread::park();
    }
}

/// set the coroutine para that passed into it
#[cold]
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
    if unlikely(!is_coroutine()) {
        return std::thread::yield_now();
    }
    let y = Yield {};
    // it's safe to use the stack value here
    yield_with(&y);
}
