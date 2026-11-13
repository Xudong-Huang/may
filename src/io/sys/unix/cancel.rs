use std::sync::Arc;

use super::EventData;
use crate::cancel::CancelIo;
use crate::scheduler::get_scheduler;
use crate::sync::AtomicOption;

pub struct CancelIoImpl(AtomicOption<Arc<EventData>>);

impl CancelIo for CancelIoImpl {
    type Data = Arc<EventData>;

    fn new() -> Self {
        CancelIoImpl(AtomicOption::none())
    }

    fn set(&self, data: Arc<EventData>) {
        self.0.store(data);
    }

    fn clear(&self) {
        self.0.take();
    }

    fn cancel(&self) -> Option<std::io::Result<()>> {
        if let Some(e) = self.0.take() {
            if let Some(co) = e.co.take() {
                // the io wait is abandoned, disarm its timer before resuming
                // the coroutine. Otherwise the timer stays armed, and if the
                // fd is reused for another timed wait it can fire and resume
                // that wait's coroutine with a spurious timeout. It may also
                // outlive this `EventData` and fire into freed memory.
                #[cfg(feature = "io_timeout")]
                e.take_timer();
                get_scheduler().schedule(co);
                return Some(Ok(()));
            }
        }
        None
    }
}
