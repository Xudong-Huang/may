//! Coroutine-aware synchronization primitives.
//!
//! Every blocking primitive works from both OS threads and may coroutines: in
//! a coroutine the operation suspends just that coroutine and yields the
//! worker thread, and (unless documented otherwise) the wait is a
//! cancellation point that unwinds with the cancel panic.
//!
//! The standard building blocks — [`Mutex`], [`RwLock`], [`Condvar`],
//! [`Barrier`] and the unbounded [`mpsc`] channel — mirror the `std::sync`
//! APIs. `may` additionally provides [`Semphore`]/[`Semaphore`],
//! [`SyncFlag`], [`WaitGroup`], [`spsc`] and [`mpmc`] channels, and the
//! low-level [`Blocker`]/[`FastBlocker`] parkers.

mod atomic_option;
mod barrier;
mod blocking;
mod condvar;
mod mutex;
mod poison;
mod rwlock;
mod semphore;
mod sync_flag;
mod wait_group;

pub(crate) mod atomic_dur;
pub(crate) mod delay_drop;
pub mod mpmc;
pub mod mpsc;
pub mod spsc;
pub use atomic_option::AtomicOption;
pub use barrier::{Barrier, BarrierWaitResult};
pub use blocking::{Blocker, FastBlocker};
pub use condvar::{Condvar, WaitTimeoutResult};
pub use mutex::{Mutex, MutexGuard};
pub use rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};
pub use semphore::{Semaphore, Semphore};
pub use sync_flag::SyncFlag;
pub use wait_group::WaitGroup;
