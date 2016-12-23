use std::sync::atomic::{AtomicBool, Ordering};

pub trait CancelIo {
    type Data;
    fn new() -> Self;
    fn set(&self, Self::Data);
    fn clear(&self);
    unsafe fn cancel(&self);
}

// each coroutine has it's own Cancel data
pub struct Cancel<T: CancelIo> {
    // true if need to cancel the coroutine
    state: AtomicBool,
    // the io data when the coroutine is suspended
    io: T,
}

// real io cancel impl is in io module
impl<T: CancelIo> Cancel<T> {
    pub fn new() -> Self {
        Cancel {
            state: AtomicBool::new(false),
            io: T::new(),
        }
    }

    // judge if the coroutine cancel flag is set
    pub fn is_canceled(&self) -> bool {
        self.state.load(Ordering::Acquire)
    }

    // async cancel for a coroutine
    pub unsafe fn cancel(&self) {
        self.state.store(true, Ordering::Release);
        self.io.cancel();
    }

    // set the cancel io data
    // should be called after register io request
    pub fn set_io(&self, io: T::Data) {
        self.io.set(io)
    }

    // clear the cancel io data
    // should be called after io completion
    pub fn clear_io(&self) {
        self.io.clear();
    }
}
