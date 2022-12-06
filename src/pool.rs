use crate::config::config;
use crate::coroutine_impl::CoroutineImpl;
use crossbeam::queue::SegQueue as Queue;
use generator::Gn;

/// the raw coroutine pool, with stack and register prepared
/// you need to tack care of the local storage
pub struct CoroutinePool {
    // the pool must support mpmc operation!
    pool: Queue<CoroutineImpl>,
}

impl CoroutinePool {
    fn create_dummy_coroutine() -> CoroutineImpl {
        Gn::new_opt(config().get_stack_size(), move || {
            unreachable!("dummy coroutine should never be called");
        })
    }

    pub fn new() -> Self {
        let capacity = config().get_pool_capacity();
        let pool = Queue::new();
        for _ in 0..capacity {
            let co = Self::create_dummy_coroutine();
            pool.push(co);
        }

        CoroutinePool { pool }
    }

    /// get a raw coroutine from the pool
    #[inline]
    pub fn get(&self) -> CoroutineImpl {
        match self.pool.pop() {
            Some(co) => co,
            None => Self::create_dummy_coroutine(),
        }
    }

    /// put a raw coroutine into the pool
    #[inline]
    pub fn put(&self, co: CoroutineImpl) {
        // discard the co if push failed
        if self.pool.len() >= config().get_pool_capacity() {
            return;
        }
        self.pool.push(co);
    }
}
