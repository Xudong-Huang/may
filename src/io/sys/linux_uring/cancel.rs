use std::io;
use std::sync::Arc;

use super::event::{EventData, CANCEL_USER};
use crate::cancel::CancelIo;
use crate::scheduler::get_scheduler;
use crate::sync::AtomicOption;

// Keeps the pending `EventData` alive via `Arc`; the cancellation completion
// resumes the coroutine only after the kernel has stopped touching its buffers.
pub struct CancelIoData {
    ev_data: Arc<EventData>,
}

impl CancelIoData {
    pub fn new(ev_data: &Arc<EventData>) -> Self {
        CancelIoData {
            ev_data: ev_data.clone(),
        }
    }

    pub fn cancel(&self) -> io::Result<()> {
        // Claim the reason before submitting. If the request is not queued
        // yet, `Selector::cancel_request` is a no-op and the adapter finishes
        // the cancellation after its push (`Selector::submit`). The fence
        // pairs with `submit`'s set_queued + fence + reason check.
        self.ev_data.mark_cancel_reason(CANCEL_USER);
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
        get_scheduler().get_selector().cancel_request(&self.ev_data)
    }
}

pub struct CancelIoImpl(AtomicOption<CancelIoData>);

impl CancelIo for CancelIoImpl {
    type Data = CancelIoData;

    fn new() -> Self {
        CancelIoImpl(AtomicOption::none())
    }

    fn set(&self, data: CancelIoData) {
        self.0.store(data);
    }

    fn clear(&self) {
        self.0.take();
    }

    fn cancel(&self) -> bool {
        // A successful `AsyncCancel` submission means the request's own
        // completion will resume the coroutine, so the cancel is handled. On
        // submission failure the coroutine stays parked until its natural
        // completion; the cancel bit still makes `yield_back` raise Cancel.
        self.0.take().is_some_and(|d| d.cancel().is_ok())
    }
}
