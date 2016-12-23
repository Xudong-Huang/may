use std::sync::Arc;
use std::cell::UnsafeCell;
use join::Join;
use coroutine::Coroutine;

// max local storage slots for coroutine
// pub const MAX_LOCAL_SLOTS: usize = 32;

pub struct CoroutineLocal {
    // storage: [*mut u8; MAX_LOCAL_SLOTS], // coroutine local storage */
    co: Coroutine, // current coroutine handle
    // when panic happens, we need to trigger the join here
    join: Arc<UnsafeCell<Join>>,
}

impl CoroutineLocal {
    /// create coroutine local data
    pub fn new(co: Coroutine, join: Arc<UnsafeCell<Join>>) -> Box<Self> {
        Box::new(CoroutineLocal {
            // storage: [ptr::null_mut(); MAX_LOCAL_SLOTS],
            co: co,
            join: join,
        })
    }

    // get the coroutine handle
    pub fn get_co(&self) -> &Coroutine {
        &self.co
    }

    // get the join handle
    pub fn get_join(&self) -> Arc<UnsafeCell<Join>> {
        self.join.clone()
    }
}
