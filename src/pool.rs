use coroutine::{DEFAULT_STACK_SIZE, CoroutineImpl};
use generator::Gn;
use queue::mpmc_bounded::Queue;

const DEFAULT_POOL_CAPACITY: usize = 100;

/// the raw coroutine pool, with stack and register prepared
/// you need to tack care of the local storage
pub struct CoroutinePool {
    // the pool must support mpmc operation!
    pool: Queue<CoroutineImpl>,
}

impl CoroutinePool {
    fn create_dummy_coroutine() -> CoroutineImpl {
        let co = Gn::new_opt(DEFAULT_STACK_SIZE, move || {
            unreachable!("dummy coroutine should never be called");
        });
        co
    }

    pub fn new() -> Self {
        let pool = Queue::with_capacity(DEFAULT_POOL_CAPACITY);
        for _ in 0..DEFAULT_POOL_CAPACITY {
            let co = Self::create_dummy_coroutine();
            pool.push(co).unwrap();
        }

        CoroutinePool { pool: pool }
    }

    /// get a raw coroutine from the pool
    #[inline]
    pub fn get(&self) -> CoroutineImpl {
        match self.pool.pop() {
            Some(co) => co,
            None => Self::create_dummy_coroutine(),
        }
    }

    /// put a raw courinte inot the pool
    #[inline]
    pub fn put(&self, co: CoroutineImpl) {
        // discard the co if push failed
        self.pool.push(co).ok();
    }
}
