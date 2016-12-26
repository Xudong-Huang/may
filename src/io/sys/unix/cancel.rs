use std::sync::Arc;
use std::sync::atomic::Ordering;
use super::EventData;
use cancel::CancelIo;
use sync::AtomicOption;
use coroutine::run_coroutine;

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
        self.0.take(Ordering::Acquire).map(|e| {
            e.co.take(Ordering::Acquire).map(run_coroutine);
        });
    }
}
