use std::sync::atomic::{AtomicUsize, Ordering};

use crate::config::config;
use crate::coroutine_impl::CoroutineImpl;
use crossbeam::queue::SegQueue;
use generator::Gn;

/// the raw coroutine pool, with stack and register prepared
/// you need to tack care of the local storage
pub struct CoroutinePool {
    // the pool must support mpmc operation!
    pool: SegQueue<CoroutineImpl>,
    size: AtomicUsize,
}

impl CoroutinePool {
    fn create_dummy_coroutine() -> CoroutineImpl {
        Gn::new_opt(config().get_stack_size(), move || {
            unreachable!("dummy coroutine should never be called");
        })
    }

    pub fn new() -> Self {
        let capacity = config().get_pool_capacity();
        let pool = SegQueue::new();
        for _ in 0..capacity {
            let co = Self::create_dummy_coroutine();
            pool.push(co);
        }
        let size = AtomicUsize::new(capacity);

        CoroutinePool { pool, size }
    }

    /// get a raw coroutine from the pool
    #[inline]
    pub fn get(&self) -> CoroutineImpl {
        self.size.fetch_sub(1, Ordering::AcqRel);
        match self.pool.pop() {
            Some(co) => co,
            None => {
                self.size.fetch_add(1, Ordering::AcqRel);
                Self::create_dummy_coroutine()
            }
        }
    }

    /// put a raw coroutine into the pool
    #[inline]
    pub fn put(&self, co: CoroutineImpl) {
        // discard the co if push failed
        let m = self.size.fetch_add(1, Ordering::AcqRel);
        if m >= config().get_pool_capacity() {
            self.size.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        self.pool.push(co);
    }
}
