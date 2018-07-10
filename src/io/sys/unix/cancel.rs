use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::EventData;
use cancel::CancelIo;
use scheduler::get_scheduler;
use sync::AtomicOption;

pub struct CancelIoImpl(AtomicOption<Arc<EventData>>);

impl CancelIo for CancelIoImpl {
    type Data = Arc<EventData>;

    fn new() -> Self {
        CancelIoImpl(AtomicOption::none())
    }

    fn set(&self, data: Arc<EventData>) {
        self.0.swap(data, Ordering::Release);
    }

    fn clear(&self) {
        self.0.take_fast(Ordering::Relaxed);
    }

    unsafe fn cancel(&self) {
        if let Some(e) = self.0.take(Ordering::Acquire) {
            if let Some(co) = e.co.take(Ordering::Acquire) {
                get_scheduler().schedule(co);
            }
        }
    }
}
