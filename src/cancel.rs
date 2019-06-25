use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use crate::coroutine_impl::CoroutineImpl;
use crate::io::cancel::CancelIoImpl;
use crate::scheduler::get_scheduler;
use crate::sync::AtomicOption;
use crate::yield_now::{get_co_para, set_co_para};
use generator::Error;

// the cancel is implemented by triggering a Cancel panic
// if drop is called due to a Cancel panic, it's not safe
// to call Any coroutine API in the drop any more because
// it would trigger another Cancel panic so here we check
// the thread panicking status
#[inline]
pub fn trigger_cancel_panic() -> ! {
    if thread::panicking() {
        eprintln!("trigger another panic while panicking");
    }

    // should we clear the cancel flag to let other API continue?
    // so that we can avoid the re-panic problem?
    // currently this is not used in any drop implementation
    // current_cancel_data().state.store(0, Ordering::Release);
    panic!(Error::Cancel);
}

pub trait CancelIo {
    type Data;
    fn new() -> Self;
    fn set(&self, _: Self::Data);
    fn clear(&self);
    unsafe fn cancel(&self);
}

// each coroutine has it's own Cancel data
pub struct CancelImpl<T: CancelIo> {
    // first bit is used when need to cancel the coroutine
    // higher bits are used to disable the cancel
    state: AtomicUsize,
    // the io data when the coroutine is suspended
    io: T,
    // other suspended type would register the co itself
    // can't set io and co at the same time!
    // most of the time this is park based API
    co: AtomicOption<Arc<AtomicOption<CoroutineImpl>>>,
}

impl<T: CancelIo> Default for CancelImpl<T> {
    fn default() -> Self {
        Self::new()
    }
}

// real io cancel impl is in io module
impl<T: CancelIo> CancelImpl<T> {
    pub fn new() -> Self {
        CancelImpl {
            state: AtomicUsize::new(0),
            io: T::new(),
            co: AtomicOption::none(),
        }
    }

    // judge if the coroutine cancel flag is set
    pub fn is_canceled(&self) -> bool {
        self.state.load(Ordering::Acquire) == 1
    }

    // return if the coroutine cancel is disabled
    pub fn is_disabled(&self) -> bool {
        self.state.load(Ordering::Acquire) >= 2
    }

    // disable the cancel bit
    pub fn disable_cancel(&self) {
        self.state.fetch_add(2, Ordering::Release);
    }

    // enable the cancel bit again
    pub fn enable_cancel(&self) {
        self.state.fetch_sub(2, Ordering::Release);
    }

    // panic if cancel was set
    pub fn check_cancel(&self) {
        if self.state.load(Ordering::Acquire) == 1 {
            // before panic clear the last coroutine error
            // this would affect future new coroutine that reuse the instance
            get_co_para();
            trigger_cancel_panic();
        }
    }

    // async cancel for a coroutine
    pub unsafe fn cancel(&self) {
        self.state.fetch_or(1, Ordering::Release);
        match self.co.take(Ordering::Acquire) {
            Some(co) => {
                co.take(Ordering::Acquire)
                    .map(|mut co| {
                        // set the cancel result for the coroutine
                        set_co_para(&mut co, io::Error::new(io::ErrorKind::Other, "Canceled"));
                        get_scheduler().schedule(co);
                    })
                    .unwrap_or(())
            }
            None => self.io.cancel(),
        }
    }

    // set the cancel io data
    // should be called after register io request
    pub fn set_io(&self, io: T::Data) {
        self.io.set(io)
    }

    // set the cancel co data
    // can't both set_io and set_co
    pub fn set_co(&self, co: Arc<AtomicOption<CoroutineImpl>>) {
        self.co.swap(co, Ordering::Release);
    }

    // clear the cancel io data
    // should be called after io completion
    pub fn clear(&self) {
        if self.co.take(Ordering::Acquire).is_none() {
            self.io.clear();
        }
    }
}

pub type Cancel = CancelImpl<CancelIoImpl>;
