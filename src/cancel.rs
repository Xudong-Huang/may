use std::cell::Cell;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use crate::coroutine_impl::CoroutineImpl;
#[cfg(feature = "io_cancel")]
use crate::io::cancel::CancelIoImpl;
use crate::likely::unlikely;
use crate::scheduler::get_scheduler;
use crate::sync::AtomicOption;
use crate::yield_now::{get_co_para, set_co_para};
use generator::Error;

// a cancel panic is in flight on this thread: a coroutine's unwind from a
// Cancel panic can be suspended (a Drop that parks), during which
// `thread::panicking()` stays true for the whole thread while other
// coroutines keep running on it. The flag distinguishes that window from a
// real (non-cancel) panic so that:
// - `poison::done` does not spuriously poison locks whose guards are
//   dropped by other coroutines during the window
// - the unwinding coroutine's own drops never re-trigger the cancel panic
thread_local! {
    static CANCEL_PANICKING: Cell<bool> = const { Cell::new(false) };
}

/// true if a Cancel-panic unwind is currently in flight on this thread
#[inline]
pub(crate) fn cancel_panic_in_flight() -> bool {
    CANCEL_PANICKING.with(|f| f.get())
}

/// mark that a Cancel-panic unwind started on this thread
/// called by `trigger_cancel_panic`, cleared when the coroutine completes
/// in `run_coroutine`
#[inline]
fn set_cancel_panic_flag() {
    CANCEL_PANICKING.with(|f| f.set(true));
}

/// clear the Cancel-panic flag once the unwind has completed
/// idempotent, called from `run_coroutine` on every coroutine completion
#[inline]
pub(crate) fn clear_cancel_panic_flag() {
    CANCEL_PANICKING.with(|f| f.set(false));
}

// the cancel is implemented by triggering a Cancel panic
// if drop is called due to a Cancel panic, it's not safe
// to call Any coroutine API in the drop any more because
// it would trigger another Cancel panic so here we check
// the thread panicking status
#[cold]
#[inline]
pub fn trigger_cancel_panic() -> ! {
    // if thread::panicking() {
    //     eprintln!("trigger another panic while panicking");
    // }
    if crate::coroutine_impl::is_coroutine() {
        set_cancel_panic_flag();
    }

    // should we clear the cancel flag to let other API continue?
    // so that we can avoid the re-panic problem?
    // currently this is not used in any drop implementation
    // current_cancel_data().state.store(0, Ordering::Release);
    std::panic::panic_any(Error::Cancel);
}

pub trait CancelIo {
    type Data;
    fn new() -> Self;
    // set the io data
    fn set(&self, io_data: Self::Data);
    // clear the io data
    fn clear(&self);
    // if io was set, return Some(io::Result<()>)
    fn cancel(&self) -> Option<io::Result<()>>;
}

#[cfg(not(feature = "io_cancel"))]
pub struct CancelIoImpl;

#[cfg(not(feature = "io_cancel"))]
impl CancelIo for CancelIoImpl {
    type Data = ();
    fn new() -> Self {
        CancelIoImpl
    }
    fn set(&self, _: Self::Data) {}
    fn clear(&self) {}
    fn cancel(&self) -> Option<io::Result<()>> {
        None
    }
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
        if unlikely(self.is_canceled()) {
            // before panic clear the last coroutine error
            // this would affect future new coroutine that reuse the instance
            get_co_para();
            // when in panic we use the stack unwind to clear resources
            if !thread::panicking() {
                trigger_cancel_panic();
            }
        }
    }

    // async cancel for a coroutine
    #[cold]
    pub fn cancel(&self) {
        self.state.fetch_or(1, Ordering::Release);

        if let Some(Ok(())) = self.io.cancel() {
            // successfully canceled
            return;
        }

        if let Some(co) = self.co.take() {
            if let Some(mut co) = co.take() {
                // this is not safe, the kernel may still need to use the overlapped
                // set the cancel result for the coroutine
                set_co_para(&mut co, io::Error::other("Canceled"));
                get_scheduler().schedule(co);
            }
        }
    }

    // set the cancel io data
    // should be called after register io request
    #[cfg(feature = "io_cancel")]
    pub fn set_io(&self, io: T::Data) {
        self.io.set(io)
    }

    // set the cancel co data
    // can't both set_io and set_co
    pub fn set_co(&self, co: Arc<AtomicOption<CoroutineImpl>>) {
        self.co.store(co);
    }

    // clear the cancel io data
    // should be called after io completion
    pub fn clear(&self) {
        self.io.clear();
    }
}

pub type Cancel = CancelImpl<CancelIoImpl>;
