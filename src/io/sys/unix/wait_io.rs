//! # Generic Wrapper for IO object
//! `wait_io` is a function that can be used in coroutine
//! context to wait on the io events
//!
use std::sync::atomic::Ordering;

use crate::cancel::Cancel;
use crate::coroutine_impl::{co_get_handle, CoroutineImpl, EventSource};
use crate::io as io_impl;
use crate::yield_now::yield_with;

pub struct RawIoBlock<'a> {
    io_data: &'a io_impl::IoData,
}

impl<'a> RawIoBlock<'a> {
    fn new(io_data: &'a io_impl::IoData) -> Self {
        RawIoBlock { io_data }
    }
}

impl<'a> EventSource for RawIoBlock<'a> {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let handle = co_get_handle(&co);
        let io_data = (*self.io_data).clone();
        self.io_data.co.swap(co, Ordering::Release);
        // there is event, re-run the coroutine
        if io_data.io_flag.load(Ordering::Acquire) {
            return io_data.schedule();
        }

        let cancel = handle.get_cancel();
        // register the cancel io data
        cancel.set_io(io_data);
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }

    /// after yield back process
    fn yield_back(&self, cancel: &'static Cancel) {
        cancel.clear_cancel_bit();
    }
}

/// This is trait that can block on io events but doing nothong about io
pub trait WaitIo {
    /// reset the io before io operation
    fn reset_io(&self);
    /// block on read/write event
    fn wait_io(&self);
}

impl<T: io_impl::AsIoData> WaitIo for T {
    fn reset_io(&self) {
        let io_data = self.as_io_data();
        io_data.reset();
    }

    fn wait_io(&self) {
        let io_data = self.as_io_data();
        // when io flag is set we do nothing
        if io_data.io_flag.load(Ordering::Relaxed) {
            return;
        }
        let blocker = RawIoBlock::new(self.as_io_data());
        yield_with(&blocker);
    }
}
