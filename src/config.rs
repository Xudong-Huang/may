
// default configs
static mut WORKERS: usize = 2;
static mut IO_WORKERS: usize = 2;
static mut POOL_CAPACITY: usize = 100;

// default stack size, in usize
// windows has a minimal size as 0x4a8!!!!
static mut STACK_SIZE: usize = 0x1000;

pub struct Config;

/// get the may configuration instance
pub fn config() -> Config {
    Config
}

/// the config should be called at the program beginning
/// successive call would not tack effect for that the scheduler
/// is already started
impl Config {
    /// set the worker thread number
    pub fn set_workers(&self, workers: usize) -> &Self {
        info!("set workers={:?}", workers);
        unsafe {
            WORKERS = workers;
        }
        self
    }

    /// get the normal workers number
    pub fn get_workers(&self) -> usize {
        unsafe { WORKERS }
    }

    /// set the io worker thread number
    pub fn set_io_workers(&self, workers: usize) -> &Self {
        info!("set io workers={:?}", workers);
        unsafe {
            IO_WORKERS = workers;
        }
        self
    }

    /// get the io workers number
    pub fn get_io_workers(&self) -> usize {
        unsafe { IO_WORKERS }
    }

    /// set coroutine pool number, 0 means unlimited
    pub fn set_pool_capacity(&self, capacity: usize) -> &Self {
        info!("set pool capacity={:?}", capacity);
        unsafe {
            POOL_CAPACITY = capacity;
        }
        self
    }

    /// get the coroutine pool capacity
    pub fn get_pool_capacity(&self) -> usize {
        unsafe { POOL_CAPACITY }
    }

    /// set default coroutine stack size in usize
    pub fn set_stack_size(&self, size: usize) -> &Self {
        info!("set stack size={:?}", size);
        unsafe {
            STACK_SIZE = size;
        }
        self
    }

    /// get the default coroutine stack size
    pub fn get_stack_size(&self) -> usize {
        unsafe { STACK_SIZE }
    }
}
