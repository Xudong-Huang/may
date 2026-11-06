//! compatible with std::sync::rwlock except for both thread and coroutine
//! please ref the doc from std::sync::rwlock
use std::cell::UnsafeCell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::sync::{LockResult, OnceLock, PoisonError, TryLockError, TryLockResult};

use crate::cancel::trigger_cancel_panic;
use crate::coroutine_impl::{current_cancel_data, is_coroutine};
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
    // below two variables consist a global mutex
    // we need to deal with the cancel logic differently
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
#[must_use]
pub struct RwLockReadGuard<'a, T: ?Sized + 'a> {
    __lock: &'a RwLock<T>,
}

// note: the write guard is `Send`/`Sync` through its fields when `T` is,
// which matches the current std behavior. The read guard only exposes
// shared access, so like std it is additionally `Sync` on `T: Sync` alone.
unsafe impl<T: ?Sized + Sync> Sync for RwLockReadGuard<'_, T> {}

/// RAII structure used to release the exclusive write access of a lock when
/// dropped.
#[must_use]
pub struct RwLockWriteGuard<'a, T: ?Sized + 'a> {
    __lock: &'a RwLock<T>,
    __poison: poison::Guard,
}

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

        let cur = SyncBlocker::current();
        // register blocker first
        self.to_wake.get_or_init(SegQueue::new).push(cur.clone());
        // inc the cnt, if it's the first grab, unpark the first waiter
        if self.cnt.fetch_add(1, Ordering::SeqCst) == 0 {
            self.to_wake
                .get_or_init(SegQueue::new)
                .pop()
                .map(|w| self.unpark_one(&w))
                .expect("got null blocker!");
        }
        match cur.park(None) {
            Ok(_) => Ok(()),
            Err(ParkError::Timeout) => unreachable!("rwlock timeout"),
            Err(ParkError::Canceled) => {
                // check the unpark status
                if cur.is_unparked() {
                    self.unlock();
                } else {
                    // register
                    cur.set_release();
                    // re-check unpark status
                    if cur.is_unparked() && cur.take_release() {
                        self.unlock();
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

    fn unlock(&self) {
        if self.cnt.fetch_sub(1, Ordering::SeqCst) > 1 {
            self.to_wake
                .get_or_init(SegQueue::new)
                .pop()
                .map(|w| self.unpark_one(&w))
                .expect("got null blocker!");
        }
    }

    fn unpark_one(&self, w: &SyncBlocker) {
        w.unpark();
        if w.take_release() {
            self.unlock();
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
        // this runs from `RwLockReadGuard::drop`, which may execute while a
        // cancel panic is unwinding the coroutine. Two hazards to avoid:
        // - parking here would suspend the unwind mid-flight, leaving
        //   `thread::panicking()` set for the whole worker thread while
        //   other coroutines run on it; their cancels would then be
        //   silently swallowed by `check_cancel`'s panicking guard and
        //   their guards spuriously poisoned
        // - if a park had to happen while the cancel bit is set, the
        //   park's subscribe would re-deliver the cancel and abort the
        //   process with a panic-while-panicking
        // so acquire the reader-count mutex without parking (see
        // `try_lock_rlock`), and keep the cancel disabled as a
        // belt-and-braces for the fallback path
        let cancel = if is_coroutine() {
            Some(current_cancel_data())
        } else {
            None
        };
        if let Some(c) = cancel.as_ref() {
            c.disable_cancel();
        }
        let mut r = if thread::panicking() {
            // we are inside an unwind (our own cancel panic, or another
            // coroutine's suspended one): avoid parking so the unwind is
            // not suspended mid-flight (see the comment above)
            self.try_lock_rlock()
        } else {
            // normal drop: the blocking lock is cheap and safe here
            self.rlock.lock().expect("rwlock read_unlock")
        };
        if let Some(c) = cancel.as_ref() {
            c.enable_cancel();
        }
        *r -= 1;
        if *r == 0 {
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
        for _ in 0..1_000_000 {
            match self.rlock.try_lock() {
                Ok(r) => return r,
                Err(TryLockError::Poisoned(_)) => {
                    // the internal mutex's critical sections never panic,
                    // so this is a spurious poison; clear and retry
                    self.rlock.clear_poison();
                    continue;
                }
                Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
            }
        }
        // pathological case: the holder is descheduled mid-section; fall
        // back to the blocking lock. the cancel is disabled around this
        // call so a re-delivered cancel cannot double-panic
        self.rlock.lock().expect("rwlock read_unlock")
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
            Err(TryLockError::Poisoned(err)) => {
                d.field("data", &&**err.get_ref())
            }
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
        poison::map_result(lock.poison.borrow(), |_| RwLockReadGuard { __lock: lock })
    }
}

impl<'rwlock, T: ?Sized> RwLockWriteGuard<'rwlock, T> {
    fn new(lock: &'rwlock RwLock<T>) -> LockResult<RwLockWriteGuard<'rwlock, T>> {
        poison::map_result(lock.poison.borrow(), |guard| RwLockWriteGuard {
            __lock: lock,
            __poison: guard,
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
    use crate::sync::mpsc::channel;
    use crate::sync::{Condvar, Mutex, RwLock};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, TryLockError};
    use std::thread;

    #[derive(Eq, PartialEq, Debug)]
    struct NonCopy(i32);

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
            Err(TryLockError::WouldBlock) => panic!("rwlock corrupted after poisoned try_write race"),
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
}
