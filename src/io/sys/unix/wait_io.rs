//! # Generic Wrapper for IO object
//! `wait_io` is a function that can be used in coroutine
//! context to wait on the io events
//!
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::cancel::Cancel;
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_get_handle;
use crate::coroutine_impl::{CoroutineImpl, EventSource};
use crate::io as io_impl;
use crate::yield_now::yield_with_io;

pub struct RawIoBlock<'a> {
    io_data: &'a io_impl::IoData,
}

impl<'a> RawIoBlock<'a> {
    fn new(io_data: &'a io_impl::IoData) -> Self {
        RawIoBlock { io_data }
    }
}

impl EventSource for RawIoBlock<'_> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        #[cfg(feature = "io_cancel")]
        let handle = co_get_handle(&co);
        let io_data = self.io_data;
        io_data.co.store(co);
        // there is event, re-run the coroutine
        if io_data.io_flag.load(Ordering::Acquire) != 0 {
            #[allow(clippy::needless_return)]
            return io_data.fast_schedule();
        }

        #[cfg(feature = "io_cancel")]
        {
            let cancel = handle.get_cancel();
            // register the cancel io data
            cancel.set_io((*io_data).clone());
            // re-check the cancel status
            if cancel.is_canceled() {
                unsafe { cancel.cancel() };
            }
        }
    }

    /// after yield back process
    fn yield_back(&self, _cancel: &'static Cancel) {
        #[cfg(feature = "io_cancel")]
        _cancel.clear_cancel_bit();
    }
}

/// A waker that could wakeup the coroutine that is blocked by `WaitIo::wait_io`
pub struct WaitIoWaker {
    io_data: Arc<io_impl::sys::EventData>,
}

impl WaitIoWaker {
    /// wakeup the coroutine that is blocked by `WaitIo::wait_io`
    pub fn wakeup(&self) {
        self.io_data
            .io_flag
            .fetch_or(0x8000_0000, Ordering::Release);
        self.io_data.schedule();
    }
}

/// This is trait that can block on io events but doing nothing about io
pub trait WaitIo {
    /// reset the io before io operation
    #[deprecated(since = "0.3.46", note = "don't need to call this method")]
    fn reset_io(&self) -> usize;
    /// block on read/write event, return events flags
    fn wait_io(&self) -> usize;
    /// wake up the coroutine that is waiting for io
    fn waker(&self) -> WaitIoWaker;
}

impl<T: io_impl::AsIoData> WaitIo for T {
    fn reset_io(&self) -> usize {
        let io_data = self.as_io_data();
        io_data.reset()
    }
    fn wait_io(&self) -> usize {
        let io_data = self.as_io_data();
        // when io flag is set we do nothing
        if io_data.io_flag.load(Ordering::Relaxed) != 0 {
            return io_data.reset();
        }
        let blocker = RawIoBlock::new(self.as_io_data());
        yield_with_io(&blocker, true);
        io_data.reset()
    }

    fn waker(&self) -> WaitIoWaker {
        WaitIoWaker {
            io_data: (*self.as_io_data()).clone(),
        }
    }
}
