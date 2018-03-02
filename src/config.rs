//! `May` Configuration interface
//!

use std::sync::atomic::{AtomicUsize, Ordering};

// default configs
const DEFAULT_WORKERS: usize = 2;
const DEFAULT_IO_WORKERS: usize = 2;
// default stack size, in usize
// windows has a minimal size as 0x4a8!!!!
const DEFAULT_STACK_SIZE: usize = 0x1000;
const DEFAULT_POOL_CAPACITY: usize = 100;

static WORKERS: AtomicUsize = AtomicUsize::new(DEFAULT_WORKERS);
static IO_WORKERS: AtomicUsize = AtomicUsize::new(DEFAULT_IO_WORKERS);
static STACK_SIZE: AtomicUsize = AtomicUsize::new(DEFAULT_STACK_SIZE);
static POOL_CAPACITY: AtomicUsize = AtomicUsize::new(DEFAULT_POOL_CAPACITY);

/// `May` Configuration type
pub struct Config;

/// get the may configuration instance
pub fn config() -> Config {
    Config
}

/// the config should be called at the program beginning
///
/// successive call would not tack effect for that the scheduler
/// is already started
impl Config {
    /// set the worker thread number
    ///
    /// the minimum worker thread is 1, if you pass 0 to it, will use internal default
    pub fn set_workers(&self, workers: usize) -> &Self {
        info!("set workers={:?}", workers);
        WORKERS.store(workers, Ordering::Release);
        self
    }

    /// get the normal workers number
    pub fn get_workers(&self) -> usize {
        let workers = WORKERS.load(Ordering::Acquire);
        if workers != 0 {
            workers
        } else {
            DEFAULT_WORKERS
        }
    }

    /// set the io worker thread number
    ///
    /// if you pass in 0, all the coroutines would be scheduled on worker thread
    pub fn set_io_workers(&self, workers: usize) -> &Self {
        info!("set io workers={:?}", workers);
        IO_WORKERS.store(workers, Ordering::Release);
        self
    }

    /// get the io workers number
    pub fn get_io_workers(&self) -> usize {
        IO_WORKERS.load(Ordering::Acquire)
    }

    /// set cached coroutine pool number
    ///
    /// if you pass 0 to it, will use internal default
    pub fn set_pool_capacity(&self, capacity: usize) -> &Self {
        info!("set pool capacity={:?}", capacity);
        POOL_CAPACITY.store(capacity, Ordering::Release);
        self
    }

    /// get the coroutine pool capacity
    pub fn get_pool_capacity(&self) -> usize {
        let size = POOL_CAPACITY.load(Ordering::Acquire);
        if size != 0 {
            size
        } else {
            DEFAULT_POOL_CAPACITY
        }
    }

    /// set default coroutine stack size in usize
    ///
    /// if you pass 0 to it, will use internal default
    pub fn set_stack_size(&self, size: usize) -> &Self {
        info!("set stack size={:?}", size);
        STACK_SIZE.store(size, Ordering::Release);
        self
    }

    /// get the default coroutine stack size
    pub fn get_stack_size(&self) -> usize {
        STACK_SIZE.load(Ordering::Acquire)
    }
}
