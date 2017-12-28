//! `May` Configuration interface
//!

use std::sync::atomic::{AtomicUsize, Ordering, ATOMIC_USIZE_INIT};

// default configs
const DEFAULT_WORKERS: usize = 2;
const DEFAULT_IO_WORKERS: usize = 2;
const DEFAULT_POOL_CAPACITY: usize = 100;
// default stack size, in usize
// windows has a minimal size as 0x4a8!!!!
const DEFAULT_STACK_SIZE: usize = 0x1000;

// TODO: update to use const fn for stable
static WORKERS: AtomicUsize = ATOMIC_USIZE_INIT;
static IO_WORKERS: AtomicUsize = ATOMIC_USIZE_INIT;
static POOL_CAPACITY: AtomicUsize = ATOMIC_USIZE_INIT;
static STACK_SIZE: AtomicUsize = ATOMIC_USIZE_INIT;

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
        WORKERS.store(workers, Ordering::Relaxed);
        self
    }

    /// get the normal workers number
    pub fn get_workers(&self) -> usize {
        match WORKERS.load(Ordering::Relaxed) {
            0 => DEFAULT_WORKERS,
            v => v,
        }
    }

    /// set the io worker thread number
    ///
    /// if you pass in 0, all the coroutines would be scheduled on worker thread
    pub fn set_io_workers(&self, workers: usize) -> &Self {
        info!("set io workers={:?}", workers);
        IO_WORKERS.store(workers, Ordering::Relaxed);
        self
    }

    /// get the io workers number
    pub fn get_io_workers(&self) -> usize {
        match IO_WORKERS.load(Ordering::Relaxed) {
            0 => DEFAULT_IO_WORKERS,
            v => v,
        }
    }

    /// set coroutine pool number, 0 means unlimited
    ///
    /// if you pass 0 to it, will use internal default
    pub fn set_pool_capacity(&self, capacity: usize) -> &Self {
        info!("set pool capacity={:?}", capacity);
        POOL_CAPACITY.store(capacity, Ordering::Relaxed);
        self
    }

    /// get the coroutine pool capacity
    pub fn get_pool_capacity(&self) -> usize {
        match IO_WORKERS.load(Ordering::Relaxed) {
            0 => DEFAULT_POOL_CAPACITY,
            v => v,
        }
    }

    /// set default coroutine stack size in usize
    ///
    /// if you pass 0 to it, will use internal default
    pub fn set_stack_size(&self, size: usize) -> &Self {
        info!("set stack size={:?}", size);
        STACK_SIZE.store(size, Ordering::Relaxed);
        self
    }

    /// get the default coroutine stack size
    pub fn get_stack_size(&self) -> usize {
        match STACK_SIZE.load(Ordering::Relaxed) {
            0 => DEFAULT_STACK_SIZE,
            v => v,
        }
    }
}
