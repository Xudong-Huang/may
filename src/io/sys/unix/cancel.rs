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

    unsafe fn cancel(&self) -> Option<std::io::Result<()>> {
        if let Some(e) = self.0.take() {
            if let Some(co) = e.co.take() {
                get_scheduler().schedule(co);
                return Some(Ok(()));
            }
        }
        None
    }
}
