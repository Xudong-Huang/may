//! # Generic Wrapper for IO object
//! `wait_io` waits for read/write readiness on the io object using a one-shot
//! `IORING_OP_POLL_ADD`.
//!
use std::sync::Arc;

use io_uring::{opcode, types};

use super::event::{EventData, IoState, CANCEL_WAKE};
use super::IoData;
#[cfg(feature = "io_cancel")]
use crate::coroutine_impl::co_cancel_data;
use crate::coroutine_impl::{CoroutineImpl, EventSource};
use crate::io as io_impl;
#[cfg(feature = "io_cancel")]
use crate::io::sys::cancel::CancelIoData;
use crate::scheduler::get_scheduler;
use crate::sync::delay_drop::DelayDrop;
use crate::yield_now::{get_co_para, yield_with_io};

/// Bit set in the flags `wait_io` returns for a forced wakeup. Selector poll
/// readiness bits only use the lower bits.
const WAKE_FLAG: usize = 0x8000_0000;

pub struct RawIoBlock {
    event_data: Arc<EventData>,
    can_drop: DelayDrop,
}

impl RawIoBlock {
    fn new(io_data: &IoData) -> Self {
        RawIoBlock {
            event_data: io_data.new_request(),
            can_drop: DelayDrop::new(),
        }
    }
}

impl EventSource for RawIoBlock {
    fn subscribe(&mut self, co: CoroutineImpl) {
        let s = get_scheduler();
        #[cfg(feature = "io_cancel")]
        let cancel = co_cancel_data(&co);
        let _g = self.can_drop.delay_drop();

        let entry = opcode::PollAdd::new(
            types::Fd(self.event_data.fd()),
            (libc::POLLIN | libc::POLLOUT) as u32,
        )
        .build();

        let event_data = submit_co!(s, &self.event_data, co, entry);

        // Close the takeover window between `wait_io`'s entry check and the
        // registration above: a waker that found no active request in that
        // window only set `wake_pending`. Convert it into a cancellation of
        // the poll we just submitted.
        if event_data.state().take_wake_pending() {
            event_data.mark_cancel_reason(CANCEL_WAKE);
            let _ = s.get_selector().cancel_request(&event_data);
        }

        #[cfg(feature = "io_cancel")]
        {
            cancel.set_io(CancelIoData::new(&event_data));
            if cancel.is_canceled() {
                cancel.cancel();
            }
        }
    }
}

/// A waker that wakes the coroutine blocked by `WaitIo::wait_io`.
pub struct WaitIoWaker {
    state: Arc<IoState>,
}

impl WaitIoWaker {
    pub fn wakeup(&self) {
        // Sticky wake for a wait that has not submitted its poll yet.
        self.state.set_wake_pending();

        if let Some(req) = self.state.peek_active() {
            req.mark_cancel_reason(CANCEL_WAKE);
            let _ = get_scheduler().get_selector().cancel_request(&req);
        }
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
        self.as_io_data().reset()
    }

    fn wait_io(&self) -> usize {
        let io_data = self.as_io_data();
        if io_data.state().take_wake_pending() {
            return WAKE_FLAG;
        }

        let blocker = RawIoBlock::new(io_data);
        yield_with_io(&blocker, true);

        let flags = blocker.event_data.result().unwrap_or(0);
        if blocker.event_data.cancel_reason() == CANCEL_WAKE {
            // Consume the `Interrupted` error the dispatcher attached for
            // non-wait_io adapters so it cannot leak into the next operation.
            let _ = get_co_para();
            return flags | WAKE_FLAG;
        }
        // A displaced concurrent wait is resumed with an error; `wait_io`
        // has no error channel, so report it as a forced wake like the
        // readiness backend does.
        if get_co_para().is_some() {
            flags | WAKE_FLAG
        } else {
            flags
        }
    }

    fn waker(&self) -> WaitIoWaker {
        WaitIoWaker {
            state: self.as_io_data().state().clone(),
        }
    }
}
