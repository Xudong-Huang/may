//! `May` Configuration interface
//!

#[cfg(feature = "io_timeout")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// default stack size, in usize
// windows has a minimal size as 0x4a8!!!!
// 0x2000 words = 64KB on 64-bit systems. Cancellation is delivered by
// unwinding with a `Cancel` panic, and on macOS arm64 a release build can
// spend roughly 43KB of the coroutine stack in the unwinder alone (libunwind
// falls back to parsing DWARF CFI with large frames for some io-wait frames).
// The previous 32KB default left no headroom there: the guard-page fault
// abandoned the unwind and left the worker thread marked as panicking.
const DEFAULT_STACK_SIZE: usize = 0x2000;
const DEFAULT_POOL_CAPACITY: usize = 1000;

#[cfg(all(target_os = "linux", feature = "io_uring"))]
const DEFAULT_IO_URING_RING_ENTRIES: usize = 1024;

static WORKERS: AtomicUsize = AtomicUsize::new(0);
static STACK_SIZE: AtomicUsize = AtomicUsize::new(DEFAULT_STACK_SIZE);
static POOL_CAPACITY: AtomicUsize = AtomicUsize::new(DEFAULT_POOL_CAPACITY);

#[cfg(all(target_os = "linux", feature = "io_uring"))]
static IO_URING_RING_ENTRIES: AtomicUsize = AtomicUsize::new(DEFAULT_IO_URING_RING_ENTRIES);

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
        info!("set workers={workers:?}");
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

    /// Set the `io_uring` submission queue size used by each io worker.
    ///
    /// This only affects builds with the `io_uring` feature on Linux. The value
    /// is rounded up to a power of two when the ring is created, and the
    /// completion queue is built with twice as many entries. Passing 0 restores
    /// the default of 1024. Like the other settings, this must be called before
    /// the scheduler starts.
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    pub fn set_io_uring_ring_entries(&self, entries: usize) -> &Self {
        info!("set io_uring ring entries={entries:?}");
        IO_URING_RING_ENTRIES.store(entries, Ordering::Release);
        self
    }

    /// Get the configured `io_uring` submission queue size.
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    pub fn get_io_uring_ring_entries(&self) -> usize {
        let entries = IO_URING_RING_ENTRIES.load(Ordering::Acquire);
        if entries != 0 {
            entries
        } else {
            DEFAULT_IO_URING_RING_ENTRIES
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
        info!("set pool capacity={capacity:?}");
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
        info!("set stack size={size:?}");
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

#[cfg(all(test, target_os = "linux", feature = "io_uring"))]
mod tests {
    use super::*;

    #[test]
    fn io_uring_ring_entries_config() {
        let config = config();
        assert_eq!(
            config.get_io_uring_ring_entries(),
            DEFAULT_IO_URING_RING_ENTRIES
        );

        config.set_io_uring_ring_entries(2048);
        assert_eq!(config.get_io_uring_ring_entries(), 2048);

        // zero restores the default, like the other config setters
        config.set_io_uring_ring_entries(0);
        assert_eq!(
            config.get_io_uring_ring_entries(),
            DEFAULT_IO_URING_RING_ENTRIES
        );
    }
}
