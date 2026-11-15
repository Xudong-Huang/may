//! # Generic Wrapper for IO object
//! `wait_io` is a function that can be used in coroutine
//! context to wait on the io events
//!
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{CoroutineImpl, EventSource};
use crate::io as io_impl;
use crate::yield_now::{get_co_para, yield_with_io};

/// Bit set in `io_flag` for a forced wakeup and in the flags `wait_io`
/// returns for it. Selector readiness bits only use the lower bits.
const WAKE_FLAG: usize = 0x8000_0000;

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
        let cancel = co_cancel_data(&co);
        let io_data = self.io_data;
        // this wait has no timeout
        let (event_data, wait) = io_data.publish_co(
            co,
            #[cfg(feature = "io_timeout")]
            None,
        );
        // there is event, re-run the coroutine
        if event_data.io_flag.load(Ordering::Acquire) != 0 {
            #[allow(clippy::needless_return)]
            return event_data.fast_schedule(&wait);
        }

        #[cfg(feature = "io_cancel")]
        {
            // register the cancel wait
            cancel.set_io(wait);
            // re-check the cancel status
            if cancel.is_canceled() {
                cancel.cancel();
            }
        }
    }
}

/// A waker that could wakeup the coroutine that is blocked by `WaitIo::wait_io`
pub struct WaitIoWaker {
    io_data: Arc<io_impl::sys::EventData>,
}

impl WaitIoWaker {
    /// wakeup the coroutine that is blocked by `WaitIo::wait_io`
    pub fn wakeup(&self) {
        self.io_data.io_flag.fetch_or(WAKE_FLAG, Ordering::Release);
        self.io_data.schedule();
    }
}

/// This is trait that can block on io events but doing nothing about io
///
/// The io object belongs to one coroutine: do not share it across threads or
/// coroutines, and never call the blocking methods (`wait_io` here,
/// `TcpListener::accept`, `UdpSocket::recv_from`, ...) from more than one
/// coroutine at the same time. A concurrent wait is detected and the earlier
/// waiter is resumed with a `concurrent io wait` error, because the later wait
/// would otherwise replace it and strand its coroutine. `wait_io` has no error
/// channel, so for it the error is dropped and the call returns as if woken by
/// `WaitIoWaker`, carrying the wake bit instead of a readiness event.
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
        // A displaced concurrent wait is resumed with a `concurrent io wait`
        // error, which `wait_io` has no error channel to report. Discard it so
        // it cannot surface on a later operation of this coroutine, and report
        // a wake without readiness: a normal wait never returns 0, so use the
        // same forced-wake bit as `WaitIoWaker`.
        let flags = io_data.reset();
        if get_co_para().is_some() {
            flags | WAKE_FLAG
        } else {
            flags
        }
    }

    fn waker(&self) -> WaitIoWaker {
        WaitIoWaker {
            io_data: (*self.as_io_data()).clone(),
        }
    }
}
