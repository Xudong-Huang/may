//! `May` Configuration interface
//!

#[cfg(feature = "io_timeout")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// default stack size, in usize
// windows has a minimal size as 0x4a8!!!!
const DEFAULT_STACK_SIZE: usize = 0x1000;
const DEFAULT_POOL_CAPACITY: usize = 1000;

static WORKERS: AtomicUsize = AtomicUsize::new(0);
static STACK_SIZE: AtomicUsize = AtomicUsize::new(DEFAULT_STACK_SIZE);
static POOL_CAPACITY: AtomicUsize = AtomicUsize::new(DEFAULT_POOL_CAPACITY);

// How long does the epoll wait before continuing with other tasks
// By default, 10ms
#[cfg(feature = "io_timeout")]
static POLL_TIMEOUT_NS: AtomicU64 = AtomicU64::new(10_000_000);

// Should cores be pinned?
static PIN_WORKERS: AtomicBool = AtomicBool::new(true);

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
        let workers = WORKERS.load(Ordering::Relaxed);
        if workers != 0 {
            workers
        } else {
            let num = num_cpus::get();
            WORKERS.store(num, Ordering::Relaxed);
            num
        }
    }

    /// set the io worker thread number
    #[deprecated(since = "0.3.13", note = "use `set_workers` only")]
    pub fn set_io_workers(&self, _workers: usize) -> &Self {
        self
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

    /// Get the current poll timeout
    #[cfg(feature = "io_timeout")]
    pub fn get_timeout_ns(&self) -> u64 {
        POLL_TIMEOUT_NS.load(Ordering::Acquire)
    }

    /// Set the poll timeout
    #[cfg(feature = "io_timeout")]
    pub fn set_timeout_ns(&self, timeout: u64) {
        POLL_TIMEOUT_NS.store(timeout, Ordering::Release);
    }

    /// Enable/Disable core/worker pinning
    pub fn set_worker_pin(&self, pin: bool) {
        PIN_WORKERS.store(pin, Ordering::Release);
    }

    /// Check if worker pinning is on
    pub fn get_worker_pin(&self) -> bool {
        PIN_WORKERS.load(Ordering::Acquire)
    }
}
