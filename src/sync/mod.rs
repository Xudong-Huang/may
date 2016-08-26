
// pub use self::mutex::{Mutex, MutexGuard};

// here we suply a mpmc chan, compatable with std::mpsc, unless the reciver half can be cloned
// pub mod mpmc;
// should we support Once? blocking for only once is fine in coroutine unless there are too much!

mod atomic_option;
// mod barrier;
// mod condvar;
// mod mutex;
// mod once;
// mod rwlock;
pub use self::atomic_option::AtomicOption;
