#[cfg(nightly)]
#[path = "atomic_option.rs"]
mod atomic_option;
#[cfg(not(nightly))]
#[path = "atomic_option_stable.rs"]
mod atomic_option;
mod blocking;
mod condvar;
mod mpsc_list;
mod mutex;
mod poison;
mod rwlock;
mod semphore;
mod sync_flag;

pub(crate) mod atomic_dur;
pub(crate) mod delay_drop;
pub mod mpmc;
pub mod mpsc;
pub use self::atomic_option::AtomicOption;
pub use self::blocking::Blocker;
pub use self::condvar::{Condvar, WaitTimeoutResult};
pub use self::mutex::{Mutex, MutexGuard};
pub use self::rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};
pub use self::semphore::Semphore;
pub use self::sync_flag::SyncFlag;
