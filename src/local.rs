use coroutine::Coroutine;

// max local storage slots for coroutine
// pub const MAX_LOCAL_SLOTS: usize = 32;

pub struct CoroutineLocal {
    co: Coroutine, /* current coroutine handle
                    * storage: [*mut u8; MAX_LOCAL_SLOTS], // coroutine local storage */
}

impl CoroutineLocal {
    /// create coroutine local data
    pub fn new(co: Coroutine) -> Box<Self> {
        Box::new(CoroutineLocal {
            co: co,
            //storage: [ptr::null_mut(); MAX_LOCAL_SLOTS],
        })
    }

    pub fn get_co(&self) -> Coroutine {
        self.co.clone()
    }
}
