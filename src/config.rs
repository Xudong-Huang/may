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

#[cfg(test)]
mod tests {
    use super::*;

    // Test utilities for configuration isolation
    mod test_utils {
        use super::*;
        use std::sync::{Mutex, OnceLock};

        static CONFIG_TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

        fn get_config_mutex() -> &'static Mutex<()> {
            CONFIG_TEST_MUTEX.get_or_init(|| Mutex::new(()))
        }

        pub struct ConfigTestGuard {
            _guard: std::sync::MutexGuard<'static, ()>,
            original_state: ConfigState,
        }

        struct ConfigState {
            workers: usize,
            stack_size: usize,
            pool_capacity: usize,
            pin_workers: bool,
            #[cfg(feature = "io_timeout")]
            poll_timeout_ns: u64,
        }

        impl ConfigTestGuard {
            pub fn new() -> Self {
                let guard = get_config_mutex().lock().unwrap();
                let cfg = config();

                let original_state = ConfigState {
                    workers: cfg.get_workers(),
                    stack_size: cfg.get_stack_size(),
                    pool_capacity: cfg.get_pool_capacity(),
                    pin_workers: cfg.get_worker_pin(),
                    #[cfg(feature = "io_timeout")]
                    poll_timeout_ns: cfg.get_timeout_ns(),
                };

                Self {
                    _guard: guard,
                    original_state,
                }
            }

            pub fn config(&self) -> Config {
                config()
            }
        }

        impl Drop for ConfigTestGuard {
            fn drop(&mut self) {
                let cfg = config();
                cfg.set_workers(self.original_state.workers);
                cfg.set_stack_size(self.original_state.stack_size);
                cfg.set_pool_capacity(self.original_state.pool_capacity);
                cfg.set_worker_pin(self.original_state.pin_workers);
                #[cfg(feature = "io_timeout")]
                cfg.set_timeout_ns(self.original_state.poll_timeout_ns);
            }
        }
    }

    #[test]
    fn test_config_creation() {
        let _cfg = config();
        // Just verify we can create a config instance
        assert!(true); // Config is a unit struct, so just verify it works
    }

    #[test]
    fn test_set_and_get_workers() {
        let cfg = config();

        // Test setting workers
        cfg.set_workers(4);
        assert_eq!(cfg.get_workers(), 4);

        // Test setting workers to 0 (should use default)
        cfg.set_workers(0);
        let workers = cfg.get_workers();
        // Should be number of CPUs since we set it to 0
        assert!(workers > 0);
        assert!(workers <= num_cpus::get());
    }

    #[test]
    fn test_set_and_get_pool_capacity() {
        let guard = test_utils::ConfigTestGuard::new();
        let cfg = guard.config();

        // Test setting pool capacity
        cfg.set_pool_capacity(500);
        assert_eq!(cfg.get_pool_capacity(), 500);

        // Test setting to 0 (should use default)
        cfg.set_pool_capacity(0);
        assert_eq!(cfg.get_pool_capacity(), DEFAULT_POOL_CAPACITY);

        // Automatic cleanup via Drop trait
    }

    #[test]
    fn test_set_and_get_stack_size() {
        let guard = test_utils::ConfigTestGuard::new();
        let cfg = guard.config();

        // Test setting stack size
        cfg.set_stack_size(8192);
        assert_eq!(cfg.get_stack_size(), 8192);

        // Test setting to 0 (should use previous value since we don't reset)
        cfg.set_stack_size(0);
        assert_eq!(cfg.get_stack_size(), 0);

        // Automatic cleanup via Drop trait
    }

    #[test]
    fn test_set_and_get_worker_pin() {
        let cfg = config();

        // Test setting worker pinning
        cfg.set_worker_pin(false);
        assert_eq!(cfg.get_worker_pin(), false);

        cfg.set_worker_pin(true);
        assert_eq!(cfg.get_worker_pin(), true);
    }

    #[test]
    #[cfg(feature = "io_timeout")]
    fn test_set_and_get_timeout_ns() {
        let cfg = config();

        // Test setting timeout
        cfg.set_timeout_ns(5_000_000); // 5ms
        assert_eq!(cfg.get_timeout_ns(), 5_000_000);

        cfg.set_timeout_ns(20_000_000); // 20ms
        assert_eq!(cfg.get_timeout_ns(), 20_000_000);
    }

    #[test]
    fn test_deprecated_set_io_workers() {
        let cfg = config();

        // Test the deprecated method still works (should be a no-op)
        #[allow(deprecated)]
        let result = cfg.set_io_workers(8);

        // Should return self for method chaining (Config is a unit struct)
        // Just verify it returns a Config instance
        let _: &Config = result;
    }

    #[test]
    fn test_method_chaining() {
        let guard = test_utils::ConfigTestGuard::new();
        let cfg = guard.config();

        // Test that methods can be chained
        let _result = cfg
            .set_workers(2)
            .set_pool_capacity(200)
            .set_stack_size(4096)
            .set_worker_pin(false);

        // Verify chaining works (Config is a unit struct)
        // Just verify it doesn't panic and methods were called

        // Verify values were set
        assert_eq!(cfg.get_workers(), 2);
        assert_eq!(cfg.get_pool_capacity(), 200);
        assert_eq!(cfg.get_stack_size(), 4096);
        assert_eq!(cfg.get_worker_pin(), false);

        // Automatic cleanup via Drop trait
    }

    #[test]
    fn test_default_constants() {
        let guard = test_utils::ConfigTestGuard::new();
        let cfg = guard.config();

        // Test that default constants have expected values
        assert_eq!(DEFAULT_POOL_CAPACITY, 1000);
        assert_eq!(DEFAULT_STACK_SIZE, 0x1000); // 4096

        // Test setting and getting specific values
        cfg.set_pool_capacity(42);
        assert_eq!(cfg.get_pool_capacity(), 42);

        cfg.set_stack_size(8192);
        assert_eq!(cfg.get_stack_size(), 8192);

        cfg.set_worker_pin(false);
        assert_eq!(cfg.get_worker_pin(), false);

        cfg.set_worker_pin(true);
        assert_eq!(cfg.get_worker_pin(), true);

        // Test that workers resolves to num_cpus when set to 0
        cfg.set_workers(0);
        let workers = cfg.get_workers();
        assert_eq!(workers, num_cpus::get());

        // Test setting specific worker count
        cfg.set_workers(5);
        assert_eq!(cfg.get_workers(), 5);

        // Automatic cleanup via Drop trait
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let cfg = Arc::new(config());
        let mut handles = vec![];

        // Test concurrent access to configuration
        for i in 0..4 {
            let cfg_clone = Arc::clone(&cfg);
            let handle = thread::spawn(move || {
                cfg_clone.set_workers(i + 1);
                cfg_clone.set_pool_capacity((i + 1) * 100);
                cfg_clone.set_stack_size((i + 1) * 1024);
                cfg_clone.set_worker_pin(i % 2 == 0);

                // Read values back
                let _workers = cfg_clone.get_workers();
                let _capacity = cfg_clone.get_pool_capacity();
                let _stack_size = cfg_clone.get_stack_size();
                let _pin = cfg_clone.get_worker_pin();
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Just verify no panics occurred
        assert!(true);
    }

    #[test]
    fn test_large_values() {
        let cfg = config();

        // Test with large values
        cfg.set_workers(1000);
        assert_eq!(cfg.get_workers(), 1000);

        cfg.set_pool_capacity(10000);
        assert_eq!(cfg.get_pool_capacity(), 10000);

        cfg.set_stack_size(1024 * 1024); // 1MB
        assert_eq!(cfg.get_stack_size(), 1024 * 1024);
    }
}
