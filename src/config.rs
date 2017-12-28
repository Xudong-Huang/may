//! `May` Configuration interface
//!

use std::sync::{Once, ONCE_INIT};
use std::sync::atomic::{AtomicUsize, Ordering, ATOMIC_USIZE_INIT};

// default configs
const DEFAULT_WORKERS: usize = 2;
const DEFAULT_IO_WORKERS: usize = 2;
// default stack size, in usize
// windows has a minimal size as 0x4a8!!!!
const DEFAULT_STACK_SIZE: usize = 0x1000;
const DEFAULT_POOL_CAPACITY: usize = 100;

static WORKERS: AtomicUsize = ATOMIC_USIZE_INIT;
static IO_WORKERS: AtomicUsize = ATOMIC_USIZE_INIT;
static STACK_SIZE: AtomicUsize = ATOMIC_USIZE_INIT;
static POOL_CAPACITY: AtomicUsize = ATOMIC_USIZE_INIT;

/// `May` Configuration type
pub struct Config;

/// get the may configuration instance
pub fn config() -> Config {
    // TODO: update to use const fn once stable
    static INIT: AtomicUsize = ATOMIC_USIZE_INIT;
    static ONCE: Once = ONCE_INIT;
    if INIT.load(Ordering::Acquire) != 0 {
        // already initialized
        return Config;
    }

    ONCE.call_once(|| {
        WORKERS.store(DEFAULT_WORKERS, Ordering::Release);
        IO_WORKERS.store(DEFAULT_IO_WORKERS, Ordering::Release);
        STACK_SIZE.store(DEFAULT_STACK_SIZE, Ordering::Release);
        POOL_CAPACITY.store(DEFAULT_POOL_CAPACITY, Ordering::Release);
        // tell that we already init to default
        INIT.store(1, Ordering::Release);
    });

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
        WORKERS.load(Ordering::Acquire)
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

    /// set coroutine pool number, 0 means unlimited
    ///
    /// if you pass 0 to it, will use internal default
    pub fn set_pool_capacity(&self, capacity: usize) -> &Self {
        info!("set pool capacity={:?}", capacity);
        POOL_CAPACITY.store(capacity, Ordering::Release);
        self
    }

    /// get the coroutine pool capacity
    pub fn get_pool_capacity(&self) -> usize {
        POOL_CAPACITY.load(Ordering::Acquire)
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
