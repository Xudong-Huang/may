//! compatible with std::sync::rwlock except for both thread and coroutine
//! please ref the doc from std::sync::rwlock
use std::cell::UnsafeCell;
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::{LockResult, OnceLock, PoisonError, TryLockError, TryLockResult};
use std::thread;

use crate::cancel::trigger_cancel_panic;
use crate::park::ParkError;
use crossbeam::queue::SegQueue;

use super::blocking::SyncBlocker;
use super::mutex::{self, Mutex, MutexGuard};
use super::poison;

/// A reader-writer lock.
///
/// This type of lock allows a number of readers or at most one writer at
/// any point in time. The write portion of this lock typically allows
/// modification of the underlying data (exclusive access) and the read
/// portion of this lock typically allows for read-only access (shared
/// access).
///
/// Works for both threads and coroutines (see the module docs): blocking
/// in a coroutine yields the underlying thread instead of blocking it, and
/// the wait can be canceled.
///
/// The priority policy of the lock is reader-preference: a new reader can
/// join already-active readers without queueing, so a writer can starve
/// under a constant stream of readers. Apart from that, waiter hand-off is
/// FIFO.
pub struct RwLock<T: ?Sized> {
    // the waiting blocker list, allocated lazily on first contention
    // so that `RwLock::new` can be a const fn
    to_wake: OnceLock<SegQueue<Arc<SyncBlocker>>>,
    // track how many blockers are waiting on the mutex
    cnt: AtomicUsize,

    // the reader mutex that track the reader count
    rlock: Mutex<usize>,

    poison: poison::Flag,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send + Sync> Send for RwLock<T> {}
unsafe impl<T: ?Sized + Send + Sync> Sync for RwLock<T> {}
impl<T: ?Sized> UnwindSafe for RwLock<T> {}
impl<T: ?Sized> RefUnwindSafe for RwLock<T> {}

/// RAII structure used to release the shared read access of a lock when
/// dropped.
///
/// Like [`std::sync::RwLockReadGuard`], the guard is `!Send`, and
/// `&RwLockReadGuard<T>` is `Sync` only when `T: Sync`.
///
/// ```compile_fail,E0277
/// fn assert_send<T: Send>() {}
/// assert_send::<may::sync::RwLockReadGuard<'static, u32>>();
/// ```
#[must_use]
pub struct RwLockReadGuard<'a, T: ?Sized + 'a> {
    __lock: &'a RwLock<T>,
    // `*mut ()` is neither `Send` nor `Sync`; it keeps the guard on the
    // locking thread. The `Sync` impl below restores `Sync` for `T: Sync`.
    _marker: PhantomData<*mut ()>,
}

// `&RwLockReadGuard<T>` only exposes `&T`, so it is `Sync` exactly when
// `T: Sync` (std's bound); the marker keeps the guard `!Send`.
unsafe impl<T: ?Sized + Sync> Sync for RwLockReadGuard<'_, T> {}

/// RAII structure used to release the exclusive write access of a lock when
/// dropped.
///
/// Like [`std::sync::RwLockWriteGuard`], the guard is `!Send`, and
/// `&RwLockWriteGuard<T>` is `Sync` only when `T: Sync`.
///
/// ```compile_fail,E0277
/// fn assert_send<T: Send>() {}
/// assert_send::<may::sync::RwLockWriteGuard<'static, u32>>();
/// ```
///
/// ```compile_fail,E0277
/// fn assert_sync<T: Sync>() {}
/// assert_sync::<may::sync::RwLockWriteGuard<'static, std::cell::Cell<u32>>>();
/// ```
#[must_use]
pub struct RwLockWriteGuard<'a, T: ?Sized + 'a> {
    __lock: &'a RwLock<T>,
    __poison: poison::Guard,
    _marker: PhantomData<*mut ()>,
}

// `&RwLockWriteGuard<T>` exposes `&T` through `Deref`, so it is `Sync` when
// `T: Sync` (std's bound); the marker keeps the guard `!Send`.
unsafe impl<T: ?Sized + Sync> Sync for RwLockWriteGuard<'_, T> {}

impl<T> RwLock<T> {
    /// Creates a new instance of an `RwLock<T>` which is unlocked.
    ///
    /// This is a `const fn`: the waiter queue is initialized lazily on
    /// first contention, so an uncontended lock never allocates.
    pub const fn new(t: T) -> RwLock<T> {
        RwLock {
            to_wake: OnceLock::new(),
            cnt: AtomicUsize::new(0),
            rlock: Mutex::new(0),
            poison: poison::Flag::new(),
            data: UnsafeCell::new(t),
        }
    }
}

impl<T: ?Sized> RwLock<T> {
    // global mutex lock without return a guard
    fn lock(&self) -> Result<(), ParkError> {
        // try lock first
        match self.try_lock() {
            Ok(_) => return Ok(()),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Poisoned(_)) => {
                unreachable!("poison is reported only after the lock is acquired")
            }
        }

        let wait_queue = self.to_wake.get_or_init(SegQueue::new);

        let cur = SyncBlocker::current();
        // register blocker first
        wait_queue.push(cur.clone());
        // inc the cnt, if it's the first grab, unpark the first waiter
        if self.cnt.fetch_add(1, Ordering::SeqCst) == 0 {
            let w = wait_queue.pop().expect("got null blocker!");
            if w.take_release() {
                self.unlock();
            } else {
                w.unpark();
            }
        }
        match cur.park(None) {
            Ok(_) => Ok(()),
            Err(ParkError::Timeout) => unreachable!("rwlock timeout"),
            Err(ParkError::Canceled) => {
                // at this point, the couroutine can come alive from
                // two possibilities: normal unpark or cancellation
                // in normal unpark we don't set_release
                if cur.is_unparked() {
                    // need to wake next waiter before we panic
                    self.unlock();
                } else {
                    // mark as canceled in the waiter queue
                    // normal unlock would skip it
                    cur.set_release();
                    // check again the release status
                    // if it becomes false means a normal unpark
                    // we need to wakeup next waiter
                    if !cur.is_released() {
                        self.unlock();
                    } else {
                        // now just leave the waiter in the wait_queue
                        // and it is marked as **released**
                        // normal `unlock()` sould pop it out with no action
                    }
                }

                Err(ParkError::Canceled)
            }
        }
    }

    fn try_lock(&self) -> TryLockResult<()> {
        // note: a failed CAS must only report `WouldBlock`. Reporting
        // `Poisoned` here would hand back a guard-in-error whose lock was
        // never acquired, and its drop would unlock a lock it never held,
        // underflowing `cnt`. Poison is reported by the guard constructors,
        // which are only called after a real acquisition.
        if self
            .cnt
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            Ok(())
        } else {
            Err(TryLockError::WouldBlock)
        }
    }

    /// Pop one waiter from the queue and unpark it
    /// until it finds one that is not canceled.
    fn unlock(&self) {
        let wait_queue = self.to_wake.get_or_init(SegQueue::new);
        let mut cnt = self.cnt.fetch_sub(1, Ordering::SeqCst);
        while cnt > 1 {
            let w = wait_queue.pop().expect("got null blocker!");
            if !w.take_release() {
                w.unpark();
                break;
            }
            cnt = self.cnt.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Locks this `RwLock` with shared read access, blocking the current
    /// thread or coroutine until it can be acquired.
    ///
    /// The calling thread or coroutine will be blocked until there are no
    /// writers which hold the lock. There may be other readers inside the
    /// lock when this method returns.
    ///
    /// For a coroutine, the wait is cancellable: if the coroutine is
    /// canceled while waiting for the lock, the cancel panic is propagated
    /// and the lock is not acquired.
    ///
    /// # Errors
    ///
    /// If the lock is poisoned, the lock is still acquired and the guard is
    /// returned inside the error; dropping the error releases the lock.
    pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        let mut r = self.rlock.lock().expect("rwlock read");
        if *r == 0 {
            if let Err(ParkError::Canceled) = self.lock() {
                // don't set the poison flag
                ::std::mem::forget(r);
                // release the mutex to let other run
                mutex::unlock_mutex(&self.rlock);
                // now we can safely go with the cancel panic
                trigger_cancel_panic();
            }
            // else the Poisoned case would be covered by the RwLockReadGuard::new()
        }
        *r += 1;
        RwLockReadGuard::new(self)
    }

    /// Attempts to acquire this `RwLock` with shared read access without
    /// blocking.
    ///
    /// If the access could not be granted at this time, then `Err` is
    /// returned. Otherwise, an RAII guard is returned which will release
    /// the shared access when it is dropped.
    ///
    /// # Errors
    ///
    /// * [`TryLockError::WouldBlock`] if the lock could not be acquired
    ///   immediately.
    /// * [`TryLockError::Poisoned`] if the lock was acquired but is
    ///   poisoned; the guard can be recovered from the error and releasing
    ///   it unlocks the lock.
    pub fn try_read(&self) -> TryLockResult<RwLockReadGuard<'_, T>> {
        let mut r = match self.rlock.try_lock() {
            Ok(r) => r,
            Err(TryLockError::Poisoned(_)) => {
                return Err(TryLockError::Poisoned(PoisonError::new(RwLockReadGuard {
                    __lock: self,
                    _marker: PhantomData,
                })));
            }
            Err(TryLockError::WouldBlock) => return Err(TryLockError::WouldBlock),
        };

        if *r == 0 {
            if let Err(TryLockError::WouldBlock) = self.try_lock() {
                return Err(TryLockError::WouldBlock);
            }
        }

        // increment the reader count before the poison check so that the
        // guard returned inside a `Poisoned` error balances its own drop
        *r += 1;
        Ok(RwLockReadGuard::new(self)?)
    }

    fn read_unlock(&self) {
        let mut r = if thread::panicking() {
            // we are inside an unwind (our own cancel panic, or another
            // coroutine's suspended one): avoid parking so the unwind is
            // not suspended mid-flight (see the comment above)
            self.try_lock_rlock()
        } else {
            // normal drop: the blocking lock is cheap and safe here
            self.rlock.lock().expect("rwlock read_unlock")
        };

        *r -= 1;
        let v = *r;

        drop(r);

        if v == 0 {
            self.unlock();
        }
    }

    /// Acquire the internal reader-count mutex without parking if possible.
    ///
    /// The mutex is only ever held for a few instructions: the read()/read_
    /// unlock() critical sections never park while holding it (a reader can
    /// only wait for the global lock after seeing *r == 0, i.e. while no
    /// read guard exists), so a bounded spin virtually always acquires.
    fn try_lock_rlock(&self) -> MutexGuard<'_, usize> {
        loop {
            match self.rlock.try_lock() {
                Ok(r) => return r,
                Err(TryLockError::Poisoned(_)) => {
                    // the internal mutex's critical sections never panic,
                    // so this is a spurious poison; clear and retry
                    self.rlock.clear_poison();
                    continue;
                }
                Err(TryLockError::WouldBlock) => crate::coroutine::yield_now(),
            }
        }
    }

    /// Locks this `RwLock` with exclusive write access, blocking the current
    /// thread or coroutine until it can be acquired.
    ///
    /// This function will not return while other writers or other readers
    /// currently have access to the lock.
    ///
    /// For a coroutine, the wait is cancellable: if the coroutine is
    /// canceled while waiting for the lock, the cancel panic is propagated
    /// and the lock is not acquired.
    ///
    /// # Errors
    ///
    /// If the lock is poisoned, the lock is still acquired and the guard is
    /// returned inside the error; dropping the error releases the lock.
    pub fn write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
        if let Err(ParkError::Canceled) = self.lock() {
            // now we can safely go with the cancel panic
            trigger_cancel_panic();
        }
        RwLockWriteGuard::new(self)
    }

    /// Attempts to lock this `RwLock` with exclusive write access without
    /// blocking.
    ///
    /// If the access could not be granted at this time, then `Err` is
    /// returned. Otherwise, an RAII guard is returned which will release
    /// the exclusive access when it is dropped.
    ///
    /// # Errors
    ///
    /// * [`TryLockError::WouldBlock`] if the lock could not be acquired
    ///   immediately.
    /// * [`TryLockError::Poisoned`] if the lock was acquired but is
    ///   poisoned; the guard can be recovered from the error and releasing
    ///   it unlocks the lock.
    pub fn try_write(&self) -> TryLockResult<RwLockWriteGuard<'_, T>> {
        if let Err(TryLockError::WouldBlock) = self.try_lock() {
            return Err(TryLockError::WouldBlock);
        }
        Ok(RwLockWriteGuard::new(self)?)
    }

    fn write_unlock(&self) {
        self.unlock();
    }

    /// Returns `true` if the lock is poisoned.
    ///
    /// A lock gets poisoned when a writer panics while holding it; reader
    /// panics and coroutine cancellation do not poison it.
    pub fn is_poisoned(&self) -> bool {
        self.poison.get()
    }

    /// Clears the poisoned state from this lock.
    ///
    /// If the lock is poisoned, it will remain poisoned until this function
    /// is called. This allows recovering from a poisoned state and marking
    /// that it has recovered.
    pub fn clear_poison(&self) {
        self.poison.clear();
    }

    /// Consumes this `RwLock`, returning the underlying data.
    ///
    /// This does not attempt to acquire the lock, so it is up to the caller
    /// to ensure the lock is not locked — in particular, no waiter may be
    /// parked in [`RwLock::read`]/[`RwLock::write`] on another reference to
    /// the lock, or that waiter would be handed the lock after the data is
    /// destroyed (undefined behavior). This is the same contract as
    /// [`std::sync::RwLock::into_inner`].
    ///
    /// # Errors
    ///
    /// If the lock is poisoned, the data is still returned, wrapped in a
    /// `PoisonError`.
    pub fn into_inner(self) -> LockResult<T>
    where
        T: Sized,
    {
        // We know statically that there are no outstanding references to
        // `self` so there's no need to lock the inner lock.
        let data = self.data.into_inner();
        poison::map_result(self.poison.borrow(), |_| data)
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `RwLock` mutably, no actual locking needs
    /// to take place — but the caller must ensure the lock is not locked
    /// (and has no parked waiters) anywhere else, e.g. by owning the only
    /// reference via `Arc::try_unwrap`. Violating this is undefined
    /// behavior.
    ///
    /// # Errors
    ///
    /// If the lock is poisoned, the data is still returned, wrapped in a
    /// `PoisonError`.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        // We know statically that there are no other references to `self`, so
        // there's no need to lock the inner lock.
        let data = unsafe { &mut *self.data.get() };
        poison::map_result(self.poison.borrow(), |_| data)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut d = f.debug_struct("RwLock");
        match self.try_read() {
            Ok(guard) => d.field("data", &&*guard),
            Err(TryLockError::Poisoned(err)) => d.field("data", &&**err.get_ref()),
            Err(TryLockError::WouldBlock) => d.field("data", &format_args!("<locked>")),
        };
        d.field("poisoned", &self.poison.get());
        d.finish_non_exhaustive()
    }
}

impl<T: Default> Default for RwLock<T> {
    /// Creates a new `RwLock<T>`, with the `Default` value for T.
    fn default() -> RwLock<T> {
        RwLock::new(Default::default())
    }
}

impl<T> From<T> for RwLock<T> {
    /// Creates a new instance of an `RwLock<T>` which is unlocked.
    /// This is equivalent to [`RwLock::new`].
    fn from(t: T) -> RwLock<T> {
        RwLock::new(t)
    }
}

impl<'rwlock, T: ?Sized> RwLockReadGuard<'rwlock, T> {
    fn new(lock: &'rwlock RwLock<T>) -> LockResult<RwLockReadGuard<'rwlock, T>> {
        poison::map_result(lock.poison.borrow(), |_| RwLockReadGuard {
            __lock: lock,
            _marker: PhantomData,
        })
    }
}

impl<'rwlock, T: ?Sized> RwLockWriteGuard<'rwlock, T> {
    fn new(lock: &'rwlock RwLock<T>) -> LockResult<RwLockWriteGuard<'rwlock, T>> {
        poison::map_result(lock.poison.borrow(), |guard| RwLockWriteGuard {
            __lock: lock,
            __poison: guard,
            _marker: PhantomData,
        })
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLockReadGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("RwLockReadGuard")
            .field("lock", &self.__lock)
            .finish_non_exhaustive()
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLockWriteGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("RwLockWriteGuard")
            .field("lock", &self.__lock)
            .finish_non_exhaustive()
    }
}

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.__lock.data.get() }
    }
}

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.__lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.__lock.data.get() }
    }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        self.__lock.read_unlock();
    }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        self.__lock.poison.done(&self.__poison);
        self.__lock.write_unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::{RwLockReadGuard, RwLockWriteGuard};
    use crate::sync::mpsc::channel;
    use crate::sync::{Condvar, Mutex, RwLock};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, TryLockError};
    use std::thread;

    #[derive(Eq, PartialEq, Debug)]
    struct NonCopy(i32);

    /// Guards are `!Send`; shared access (`&guard`) is `Sync` for `T: Sync`
    /// only. The negative bounds are checked by compile_fail doctests.
    #[test]
    fn guard_trait_bounds() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<RwLockReadGuard<'static, u32>>();
        assert_sync::<RwLockWriteGuard<'static, u32>>();
        // std parity: `Sync` does not require `Send`
        assert_sync::<RwLockReadGuard<'static, std::sync::MutexGuard<'static, u32>>>();
        assert_sync::<RwLockWriteGuard<'static, std::sync::MutexGuard<'static, u32>>>();
    }

    #[test]
    fn smoke() {
        let l = RwLock::new(());
        drop(l.read().unwrap());
        drop(l.write().unwrap());
        drop((l.read().unwrap(), l.read().unwrap()));
        drop(l.write().unwrap());
    }

    #[test]
    fn frob() {
        const N: usize = 10;
        const M: usize = 1000;

        let r = Arc::new(RwLock::new(()));

        let (tx, rx) = channel::<()>();
        for i in 0..N {
            let tx = tx.clone();
            let r = r.clone();
            let f = move || {
                for i in 0..M {
                    if i % 5 == 0 {
                        drop(r.write().unwrap());
                    } else {
                        drop(r.read().unwrap());
                    }
                }
                drop(tx);
            };
            if i % 2 == 0 {
                go!(f);
            } else {
                thread::spawn(f);
            }
        }
        drop(tx);
        let _ = rx.recv();
    }

    #[test]
    fn test_rw_arc_poison_wr() {
        let arc = Arc::new(RwLock::new(1));
        let arc2 = arc.clone();
        let _: Result<(), _> = thread::spawn(move || {
            let _lock = arc2.write().unwrap();
            panic!();
        })
        .join();
        assert!(arc.read().is_err());
    }

    #[test]
    fn test_rw_arc_poison_ww() {
        let arc = Arc::new(RwLock::new(1));
        assert!(!arc.is_poisoned());
        let arc2 = arc.clone();
        let _: Result<(), _> = thread::spawn(move || {
            let _lock = arc2.write().unwrap();
            panic!();
        })
        .join();
        assert!(arc.write().is_err());
        assert!(arc.is_poisoned());
    }

    #[test]
    fn test_rw_arc_no_poison_rr() {
        let arc = Arc::new(RwLock::new(1));
        let arc2 = arc.clone();
        let _: Result<(), _> = thread::spawn(move || {
            let _lock = arc2.read().unwrap();
            panic!();
        })
        .join();
        let lock = arc.read().unwrap();
        assert_eq!(*lock, 1);
    }
    #[test]
    fn test_rw_arc_no_poison_rw() {
        let arc = Arc::new(RwLock::new(1));
        let arc2 = arc.clone();
        let _: Result<(), _> = thread::spawn(move || {
            let _lock = arc2.read().unwrap();
            panic!()
        })
        .join();
        let lock = arc.write().unwrap();
        assert_eq!(*lock, 1);
    }

    #[test]
    fn test_rw_arc() {
        let arc = Arc::new(RwLock::new(0));
        let arc2 = arc.clone();
        let (tx, rx) = channel();

        thread::spawn(move || {
            let mut lock = arc2.write().unwrap();
            for _ in 0..10 {
                let tmp = *lock;
                *lock = -1;
                thread::yield_now();
                *lock = tmp + 1;
            }
            tx.send(()).unwrap();
        });

        // Readers try to catch the writer in the act
        let mut children = Vec::new();
        for _ in 0..5 {
            let arc3 = arc.clone();
            children.push(thread::spawn(move || {
                let lock = arc3.read().unwrap();
                assert!(*lock >= 0);
            }));
        }

        // Wait for children to pass their asserts
        for r in children {
            assert!(r.join().is_ok());
        }

        // Wait for writer to finish
        rx.recv().unwrap();
        let lock = arc.read().unwrap();
        assert_eq!(*lock, 10);
    }

    #[test]
    fn test_rw_arc_access_in_unwind() {
        let arc = Arc::new(RwLock::new(1));
        let arc2 = arc.clone();
        let _ = thread::spawn(move || {
            struct Unwinder {
                i: Arc<RwLock<isize>>,
            }
            impl Drop for Unwinder {
                fn drop(&mut self) {
                    let mut lock = self.i.write().unwrap();
                    *lock += 1;
                }
            }
            let _u = Unwinder { i: arc2 };
            panic!();
        })
        .join();
        let lock = arc.read().unwrap();
        assert_eq!(*lock, 2);
    }

    #[test]
    fn test_rwlock_unsized() {
        let rw: &RwLock<[i32]> = &RwLock::new([1, 2, 3]);
        {
            let b = &mut *rw.write().unwrap();
            b[0] = 4;
            b[2] = 5;
        }
        let comp: &[i32] = &[4, 2, 5];
        assert_eq!(&*rw.read().unwrap(), comp);
    }

    #[test]
    fn test_rwlock_try_write() {
        let lock = RwLock::new(0isize);
        let read_guard = lock.read().unwrap();

        let write_result = lock.try_write();
        match write_result {
            Err(TryLockError::WouldBlock) => (),
            Ok(_) => panic!("try_write should not succeed while read_guard is in scope"),
            Err(_) => panic!("unexpected error"),
        }

        drop(read_guard);
    }

    #[test]
    fn test_try_read_poisoned_free() {
        // poison the rwlock
        let rw = Arc::new(RwLock::new(1));
        let rw2 = rw.clone();
        let _: Result<(), _> = thread::spawn(move || {
            let _lock = rw2.write().unwrap();
            panic!();
        })
        .join();
        assert!(rw.is_poisoned());

        // a poisoned try_read must report Poisoned with the lock held;
        // dropping the error must release the lock again
        drop(rw.try_read().unwrap_err());

        // the lock must still be usable: acquired with a poisoned report
        match rw.try_write() {
            Err(TryLockError::Poisoned(_)) => {}
            Err(TryLockError::WouldBlock) => panic!("rwlock corrupted after poisoned try_read"),
            Ok(_) => panic!("try_write should report poison"),
        };
    }

    #[test]
    fn test_poisoned_race_no_underflow() {
        use std::sync::Barrier;

        let rw = Arc::new(RwLock::new(1));
        let rw2 = rw.clone();
        let _: Result<(), _> = thread::spawn(move || {
            let _lock = rw2.write().unwrap();
            panic!();
        })
        .join();
        assert!(rw.is_poisoned());

        // threads race try_write on a poisoned free rwlock; the loser
        // of the CAS must get WouldBlock, not a guard it never acquired
        for _ in 0..2000 {
            let barrier = Arc::new(Barrier::new(8));
            let mut hs = vec![];
            for _ in 0..8 {
                let rw = rw.clone();
                let b = barrier.clone();
                hs.push(thread::spawn(move || {
                    b.wait();
                    drop(rw.try_write());
                }));
            }
            for h in hs {
                h.join().unwrap();
            }
        }

        // the lock must still be usable
        match rw.try_write() {
            Err(TryLockError::Poisoned(_)) => {}
            Err(TryLockError::WouldBlock) => {
                panic!("rwlock corrupted after poisoned try_write race")
            }
            Ok(_) => panic!("try_write should report poison"),
        };
    }

    #[test]
    fn test_into_inner() {
        let m = RwLock::new(NonCopy(10));
        assert_eq!(m.into_inner().unwrap(), NonCopy(10));
    }

    #[test]
    fn test_into_inner_drop() {
        struct Foo(Arc<AtomicUsize>);
        impl Drop for Foo {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let num_drops = Arc::new(AtomicUsize::new(0));
        let m = RwLock::new(Foo(num_drops.clone()));
        assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        {
            let _inner = m.into_inner().unwrap();
            assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        }
        assert_eq!(num_drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_into_inner_poison() {
        let m = Arc::new(RwLock::new(NonCopy(10)));
        let m2 = m.clone();
        let _ = thread::spawn(move || {
            let _lock = m2.write().unwrap();
            panic!("test panic in inner thread to poison RwLock");
        })
        .join();

        assert!(m.is_poisoned());
        match Arc::try_unwrap(m).unwrap().into_inner() {
            Err(e) => assert_eq!(e.into_inner(), NonCopy(10)),
            Ok(x) => panic!("into_inner of poisoned RwLock is Ok: {x:?}"),
        }
    }

    #[test]
    fn test_get_mut() {
        let mut m = RwLock::new(NonCopy(10));
        *m.get_mut().unwrap() = NonCopy(20);
        assert_eq!(m.into_inner().unwrap(), NonCopy(20));
    }

    #[test]
    fn test_get_mut_poison() {
        let m = Arc::new(RwLock::new(NonCopy(10)));
        let m2 = m.clone();
        let _ = thread::spawn(move || {
            let _lock = m2.write().unwrap();
            panic!("test panic in inner thread to poison RwLock");
        })
        .join();

        assert!(m.is_poisoned());
        match Arc::try_unwrap(m).unwrap().get_mut() {
            Err(e) => assert_eq!(*e.into_inner(), NonCopy(10)),
            Ok(x) => panic!("get_mut of poisoned RwLock is Ok: {x:?}"),
        }
    }

    #[test]
    fn test_const_new() {
        // `static` requires a const initializer, so this proves
        // `RwLock::new` is a `const fn`
        static RW: RwLock<i32> = RwLock::new(1);
        let mut g = RW.write().unwrap();
        *g += 1;
        drop(g);
        assert_eq!(*RW.read().unwrap(), 2);
    }

    #[test]
    fn test_clear_poison() {
        let rw = Arc::new(RwLock::new(NonCopy(10)));
        let rw2 = rw.clone();
        let _ = thread::spawn(move || {
            let _lock = rw2.write().unwrap();
            panic!("test panic in inner thread to poison RwLock");
        })
        .join();

        assert!(rw.is_poisoned());
        rw.clear_poison();
        assert!(!rw.is_poisoned());
        assert_eq!(*rw.read().unwrap(), NonCopy(10));
    }

    #[test]
    fn test_from() {
        let rw = RwLock::from(NonCopy(10));
        assert_eq!(rw.into_inner().unwrap(), NonCopy(10));
    }

    #[test]
    fn test_read_guard_sync_bound() {
        fn assert_sync<T: Sync>() {}
        // `&dyn Sync` is Sync but not Send: the read guard only exposes
        // shared access, so like std it must be Sync on the `T: Sync`
        // bound alone, without requiring `T: Send + Sync` via the lock
        assert_sync::<super::RwLockReadGuard<'static, &'static dyn Sync>>();
    }

    #[test]
    fn test_rwlock_write_canceled() {
        const N: usize = 10;

        let sync = Arc::new((Mutex::new(0), Condvar::new()));
        let (tx, rx) = channel();
        let mut vec = vec![];
        let rwlock = Arc::new(RwLock::new(0));
        for i in 1..N + 1 {
            // println!("create thread id={}", i);
            let sync = sync.clone();
            let tx = tx.clone();
            let rwlock = rwlock.clone();
            let h = go!(move || {
                // tell master that we started
                tx.send(0).unwrap();
                // first get the wlock
                let _wlock = rwlock.write().unwrap();
                tx.send(i).unwrap();
                // println!("got wlock, id={}", i);

                // wait the mater to let it go
                let (lock, cond) = &*sync;
                let mut cnt = lock.lock().unwrap();
                while *cnt != i {
                    cnt = cond.wait(cnt).unwrap();
                }
                // println!("got cond id={}", i);
            });
            vec.push(h);
        }
        drop(tx);

        // wait for coroutine started
        let mut id = 0;
        for _ in 1..N + 2 {
            let a = rx.recv().unwrap();
            if a != 0 {
                id = a;
                // first recv one id
                // println!("recv id = {}", id);
            }
        }

        // cancel one coroutine that is waiting for the rwlock
        let mut cancel_id = id + 1;
        if cancel_id == N + 1 {
            cancel_id = 1;
        }
        // println!("cancel id = {}", cancel_id);
        vec[cancel_id - 1].coroutine().cancel();

        // let all coroutine to continue
        let (lock, cond) = &*sync;
        for _ in 1..N {
            let mut cnt = lock.lock().unwrap();
            *cnt = id;
            cond.notify_all();
            drop(cnt);
            id = rx.recv().unwrap_or(0);
            // println!("recv id = {:?}", id);
        }

        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_rwlock_read_canceled() {
        let (tx, rx) = channel();
        let rwlock = Arc::new(RwLock::new(0));

        // lock the write lock so all reader lock would enqueue
        let wlock = rwlock.write().unwrap();

        // create a coroutine that use reader locks
        let h = {
            let tx = tx.clone();
            let rwlock = rwlock.clone();
            go!(move || {
                // tell master that we started
                tx.send(0).unwrap();
                // first get the rlock
                let _rlock = rwlock.read().unwrap();
                tx.send(1).unwrap();
            })
        };

        // wait for reader coroutine started
        let a = rx.recv().unwrap();
        assert_eq!(a, 0);

        // create another thread that wait for wlock
        let rwlock1 = rwlock.clone();
        let tx1 = tx.clone();
        thread::spawn(move || {
            let _wlock = rwlock1.write().unwrap();
            tx1.send(10).unwrap();
        });

        // cancel read coroutine that is waiting for the rwlock
        h.coroutine().cancel();
        h.join().unwrap_err();

        // release the write lock, so that other thread can got the lock
        drop(wlock);
        let a = rx.recv().unwrap();
        assert_eq!(a, 10);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_rwlock_write_held_cancel() {
        use crate::sleep::sleep;
        use std::time::Duration;

        let rw1 = Arc::new(RwLock::new(0));
        let rw2 = rw1.clone();
        let rw3 = rw1.clone();

        let h1 = go!(move || {
            let mut g = rw2.write().unwrap();
            // cancel while holding the write lock
            // this should not poison the lock
            sleep(Duration::from_secs(10000));
            *g += 1;
        });

        let h2 = go!(move || {
            // let h1 grab the lock first
            sleep(Duration::from_millis(50));
            let mut g = rw3.write().unwrap();
            *g += 1;
        });

        // let h1 hold the write lock
        sleep(Duration::from_millis(100));
        // cancel h1 while it holds the lock
        h1.coroutine().cancel();
        h1.join().unwrap_err();
        h2.join().unwrap();

        assert!(!rw1.is_poisoned());
        let g = rw1.read().unwrap();
        assert_eq!(*g, 1);
    }

    #[test]
    fn test_rwlock_read_held_cancel() {
        use crate::sleep::sleep;
        use std::time::Duration;

        let rw1 = Arc::new(RwLock::new(0));
        let rw2 = rw1.clone();

        let h1 = go!(move || {
            let _g = rw2.read().unwrap();
            // cancel while holding the read lock
            sleep(Duration::from_secs(10000));
        });

        // let h1 grab the read lock
        sleep(Duration::from_millis(100));
        h1.coroutine().cancel();
        h1.join().unwrap_err();

        // the read guard must have been released during the unwind and
        // the lock must not be poisoned
        assert!(!rw1.is_poisoned());
        let mut g = rw1.write().unwrap();
        *g += 1;
        assert_eq!(*g, 1);
    }

    #[test]
    fn test_rwlock_read_drop_cancel_stress() {
        use crate::sleep::sleep;
        use std::time::Duration;

        let rw = Arc::new(RwLock::new(0usize));

        // a thread that continuously hammers read() to keep the internal
        // reader-count mutex contended while read guards drop during cancel
        // unwinds. read() barges past the held read lock instead of
        // blocking (write() would park on the global lock and stop the
        // hammer), so it keeps hitting the reader-count mutex the whole
        // time the coroutine below holds its guard.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let rw_hammer = rw.clone();
        let hammer = thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                drop(rw_hammer.read().unwrap());
            }
        });

        for _ in 0..100 {
            let rw_co = rw.clone();
            let h = go!(move || {
                let _g = rw_co.read().unwrap();
                // cancel while holding the read lock, racing the hammer
                // thread for the internal reader-count mutex during the
                // guard drop
                sleep(Duration::from_millis(50));
            });
            sleep(Duration::from_millis(1));
            h.coroutine().cancel();
            h.join().unwrap_err();
        }

        stop.store(true, Ordering::Relaxed);
        hammer.join().unwrap();

        let mut g = rw.write().unwrap();
        *g += 1;
    }

    /// Regression test for the hand-off chain: a cancel storm leaves many
    /// released blockers queued, and unlocking must walk past all of them
    /// iteratively. The old implementation recursed once per released
    /// waiter and overflowed the 32KB coroutine stack on a chain this long.
    #[test]
    fn test_rwlock_unlock_skips_canceled_waiters() {
        const N: usize = 500;

        let rw = Arc::new(RwLock::new(0usize));
        let (ready_tx, ready_rx) = channel();
        let (release_tx, release_rx) = channel();

        // hold the write lock from a coroutine so the hand-off on drop runs
        // on a coroutine stack
        let rw_holder = rw.clone();
        let holder = go!(move || {
            let _g = rw_holder.write().unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            // `_g` drops here, performing the hand-off
        });
        ready_rx.recv().unwrap();

        let waiters: Vec<_> = (0..N)
            .map(|_| {
                let rw = rw.clone();
                go!(move || {
                    drop(rw.write().unwrap());
                })
            })
            .collect();

        // let the waiters park, then cancel them all; each leaves a released
        // blocker in the hand-off queue
        std::thread::sleep(std::time::Duration::from_millis(100));
        for w in &waiters {
            w.coroutine().cancel();
        }
        for w in waiters {
            assert!(w.join().is_err());
        }

        // dropping the guard now walks the released chain on a 32KB stack
        release_tx.send(()).unwrap();
        holder.join().unwrap();

        // the lock must be usable afterwards
        assert!(!rw.is_poisoned());
        let mut g = rw.write().unwrap();
        *g += 1;
        assert_eq!(*g, 1);
    }
}
