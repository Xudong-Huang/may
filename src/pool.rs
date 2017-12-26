use generator::Gn;
use config::config;
use coroutine_impl::CoroutineImpl;
use may_queue::mpmc_bounded::Queue;

/// the raw coroutine pool, with stack and register prepared
/// you need to take care of the local storage
pub struct CoroutinePool {
    // the pool must support mpmc operation!
    pool: Queue<CoroutineImpl>,
}

impl CoroutinePool {
    fn create_dummy_coroutine() -> CoroutineImpl {
        let co = Gn::new_opt(config().get_stack_size(), move || {
            unreachable!("dummy coroutine should never be called");
        });
        co
    }

    pub fn new() -> Self {
        let capacity = config().get_pool_capacity();
        let pool = Queue::with_capacity(capacity);
        for _ in 0..capacity {
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

    /// put a raw coroutine into the pool
    #[inline]
    pub fn put(&self, co: CoroutineImpl) {
        // discard the co if push failed
        self.pool.push(co).ok();
    }
}
