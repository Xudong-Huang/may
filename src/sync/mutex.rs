//! compatible with std::sync::mutex except for both thread and coroutine
//! please ref the doc from std::sync::mutex
use std::cell::UnsafeCell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::{LockResult, OnceLock, TryLockError, TryLockResult};

use super::blocking::SyncBlocker;
use super::poison;
use crate::cancel::trigger_cancel_panic;
use crate::park::ParkError;

use may_queue::mpsc::Queue;

/// A mutual exclusion primitive useful for protecting shared data.
///
/// Works for both threads and coroutines (see the module docs): blocking in
/// a coroutine yields the underlying thread instead of blocking it, and the
/// wait can be canceled. Waiter hand-off is FIFO.
pub struct Mutex<T: ?Sized> {
    // the waiting blocker list, allocated lazily on first contention
    // so that `Mutex::new` can be a const fn
    to_wake: OnceLock<Queue<Arc<SyncBlocker>>>,
    // track how many blockers are waiting on the mutex
    cnt: AtomicUsize,
    poison: poison::Flag,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}
impl<T: ?Sized> UnwindSafe for Mutex<T> {}
impl<T: ?Sized> RefUnwindSafe for Mutex<T> {}

pub struct MutexGuard<'a, T: ?Sized + 'a> {
    // funny underscores due to how Deref/DerefMut currently work (they
    // disregard field privacy).
    __lock: &'a Mutex<T>,
    __poison: poison::Guard,
}

// note: MutexGuard is `Send`/`Sync` through its fields when `T` is, which
// matches the current std behavior.

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    ///
    /// This is a `const fn`: the waiter queue is initialized lazily on
    /// first contention, so an uncontended mutex never allocates.
    pub const fn new(t: T) -> Mutex<T> {
        Mutex {
            to_wake: OnceLock::new(),
            cnt: AtomicUsize::new(0),
            poison: poison::Flag::new(),
            data: UnsafeCell::new(t),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquires the mutex, blocking the current thread or coroutine until it
    /// is available.
    ///
    /// For a coroutine, the wait is cancellable: if the coroutine is
    /// canceled while waiting for the lock, the cancel panic is propagated
    /// and the lock is not acquired.
    ///
    /// # Errors
    ///
    /// If another user of this mutex panicked while holding it, then this
    /// call returns an error once the mutex is acquired. The lock is still
    /// acquired in that case; recover the guard from the error and dropping
    /// it unlocks the mutex.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        // try lock first
        match self.try_lock() {
            Ok(g) => return Ok(g),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Poisoned(e)) => return Err(e),
        }

        let cur = SyncBlocker::current();
        // register blocker first
        self.to_wake.get_or_init(Queue::new).push(cur.clone());
        // inc the cnt, if it's the first grab, unpark the first waiter
        if self.cnt.fetch_add(1, Ordering::SeqCst) == 0 {
            let w = self
                .to_wake
                .get_or_init(Queue::new)
                .pop()
                .expect("got null blocker!");
            if self.unpark_one(&w) {
                self.unlock();
            }
        }
        loop {
            match cur.park(None) {
                Ok(_) => {
                    break;
                }
                Err(ParkError::Timeout) => unreachable!("mutex timeout"),
                Err(ParkError::Canceled) => {
                    let b_ignore = if crate::coroutine_impl::is_coroutine() {
                        let cancel = crate::coroutine_impl::current_cancel_data();
                        cancel.is_disabled()
                    } else {
                        false
                    };
                    // check the unpark status
                    if cur.is_unparked() {
                        if b_ignore {
                            break;
                        }
                        self.unlock();
                    } else if !b_ignore {
                        // register the release action: if we get handed the
                        // lock while unwinding, pass it on to the next waiter
                        cur.set_release();
                        // re-check unpark status
                        if cur.is_unparked() && cur.take_release() {
                            self.unlock();
                        }
                    }
                    // we ignore the cancel, just to wait the actual event
                    if b_ignore {
                        continue;
                    }

                    // now we can safely go with the cancel panic
                    trigger_cancel_panic();
                }
            }
        }

        MutexGuard::new(self)
    }

    /// Attempts to acquire the mutex without blocking.
    ///
    /// # Errors
    ///
    /// * [`TryLockError::WouldBlock`] if the lock is currently held by
    ///   another thread or coroutine.
    /// * [`TryLockError::Poisoned`] if the lock was acquired but is
    ///   poisoned; the guard can be recovered from the error and unlocking
    ///   happens when the error is dropped.
    pub fn try_lock(&self) -> TryLockResult<MutexGuard<'_, T>> {
        match self
            .cnt
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
        {
            Ok(_) => Ok(MutexGuard::new(self)?),
            Err(_) => Err(TryLockError::WouldBlock),
        }
    }

    /// unpark one waiter, return true if it registered a release action:
    /// it is unwinding (canceled) and won't take the lock, so the hand-off
    /// must continue with the next waiter
    fn unpark_one(&self, w: &SyncBlocker) -> bool {
        w.unpark();
        w.take_release()
    }

    fn unlock(&self) {
        // pass the lock down the waiter chain iteratively: a popped waiter
        // that registered a release action is unwinding and won't take the
        // lock, so the hand-off continues with the next waiter. Recursing
        // here would overflow the (small, 32KB default) coroutine stack
        // when a cancel storm leaves a chain of released waiters (see the
        // same fix in condvar::notify_one).
        let mut cnt = self.cnt.fetch_sub(1, Ordering::SeqCst);
        while cnt > 1 {
            let w = self
                .to_wake
                .get_or_init(Queue::new)
                .pop()
                .expect("got null blocker!");
            if !self.unpark_one(&w) {
                break;
            }
            // the released waiter's slot is released as well
            cnt = self.cnt.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Returns `true` if the mutex is poisoned.
    ///
    /// A mutex gets poisoned when a panic unwinds through a guard that was
    /// not caused by a coroutine cancellation.
    #[inline]
    pub fn is_poisoned(&self) -> bool {
        self.poison.get()
    }

    /// Clears the poisoned state from this mutex.
    ///
    /// If the mutex is poisoned, it will remain poisoned until this function
    /// is called. This allows recovering from a poisoned state and marking
    /// that it has recovered. For example, if the value is overwritten by a
    /// known-good value, then the mutex can be marked as un-poisoned. Or
    /// possibly, the value could be inspected to determine if it is in a
    /// consistent state, and if so the poison is removed.
    pub fn clear_poison(&self) {
        self.poison.clear();
    }

    /// Consumes this mutex, returning the underlying data.
    ///
    /// This does not attempt to acquire the lock, so it is up to the caller
    /// to ensure the mutex is not locked — in particular, no waiter may be
    /// parked in [`Mutex::lock`] on another reference to the mutex, or that
    /// waiter would be handed the lock after the data is destroyed
    /// (undefined behavior). This is the same contract as
    /// [`std::sync::Mutex::into_inner`].
    ///
    /// # Errors
    ///
    /// If the mutex is poisoned, the data is still returned, wrapped in a
    /// `PoisonError`.
    pub fn into_inner(self) -> LockResult<T>
    where
        T: Sized,
    {
        let data = self.data.into_inner();
        poison::map_result(self.poison.borrow(), |_| data)
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `Mutex` mutably, no actual locking needs
    /// to take place — but the caller must ensure the mutex is not locked
    /// (and has no parked waiters) anywhere else, e.g. by owning the only
    /// reference via `Arc::try_unwrap`. Violating this is undefined
    /// behavior.
    ///
    /// # Errors
    ///
    /// If the mutex is poisoned, the data is still returned, wrapped in a
    /// `PoisonError`.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        // We know statically that there are no other references to `self`, so
        // there's no need to lock the inner lock.
        let data = unsafe { &mut *self.data.get() };
        poison::map_result(self.poison.borrow(), |_| data)
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Mutex<T> {
        Mutex::new(Default::default())
    }
}

impl<T> From<T> for Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    /// This is equivalent to [`Mutex::new`].
    fn from(t: T) -> Mutex<T> {
        Mutex::new(t)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.try_lock() {
            Ok(guard) => write!(f, "Mutex {{ data: {:?} }}", &*guard),
            Err(TryLockError::Poisoned(err)) => {
                write!(f, "Mutex {{ data: Poisoned({:?}) }}", &**err.get_ref())
            }
            Err(TryLockError::WouldBlock) => write!(f, "Mutex {{ <locked> }}"),
        }
    }
}

impl<'mutex, T: ?Sized> MutexGuard<'mutex, T> {
    fn new(lock: &'mutex Mutex<T>) -> LockResult<MutexGuard<'mutex, T>> {
        // the acquire ordering is provided by the SeqCst operations on
        // `cnt` in the lock/unlock paths, no extra fence is needed
        poison::map_result(lock.poison.borrow(), |guard| MutexGuard {
            __lock: lock,
            __poison: guard,
        })
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.__lock.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.__lock.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.__lock.poison.done(&self.__poison);
        // the SeqCst fetch_sub in `unlock` provides the release ordering
        self.__lock.unlock();
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("MutexGuard")
            .field("lock", &self.__lock)
            .finish()
    }
}

// below functions are used by condvar but not exported to user
pub fn unlock_mutex<T: ?Sized>(lock: &Mutex<T>) {
    lock.unlock();
}

pub fn guard_lock<'a, T: ?Sized>(guard: &MutexGuard<'a, T>) -> &'a Mutex<T> {
    guard.__lock
}

pub fn guard_poison<'a, T: ?Sized>(guard: &MutexGuard<'a, T>) -> &'a poison::Flag {
    &guard.__lock.poison
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::mpsc::channel;
    use crate::sync::Condvar;
    use std::thread;

    struct Packet<T>(Arc<(Mutex<T>, Condvar)>);
    unsafe impl<T: Send> Send for Packet<T> {}
    unsafe impl<T> Sync for Packet<T> {}

    #[derive(Eq, PartialEq, Debug)]
    struct NonCopy(i32);

    #[test]
    fn smoke() {
        let m = Mutex::new(());
        drop(m.lock().unwrap());
        drop(m.lock().unwrap());
    }

    #[test]
    fn test_const_new() {
        // `static` requires a const initializer, so this proves
        // `Mutex::new` is a `const fn`
        static M: Mutex<i32> = Mutex::new(1);
        let mut g = M.lock().unwrap();
        *g += 1;
        drop(g);
        assert_eq!(*M.lock().unwrap(), 2);
    }

    #[test]
    fn lots_and_lots() {
        const J: u32 = 1000;
        const K: u32 = 3;

        let m = Arc::new(Mutex::new(0));

        fn inc(m: &Mutex<u32>) {
            for _ in 0..J {
                *m.lock().unwrap() += 1;
            }
        }

        let (tx, rx) = channel();
        for _ in 0..K {
            let tx2 = tx.clone();
            let m2 = m.clone();
            thread::spawn(move || {
                inc(&m2);
                tx2.send(()).unwrap();
            });
            let tx2 = tx.clone();
            let m2 = m.clone();
            go!(move || {
                inc(&m2);
                tx2.send(()).unwrap();
            });
        }

        drop(tx);
        for _ in 0..2 * K {
            rx.recv().unwrap();
        }
        assert_eq!(*m.lock().unwrap(), J * K * 2);
    }

    #[test]
    fn try_lock() {
        let m = Mutex::new(());
        *m.try_lock().unwrap() = ();
    }

    #[test]
    fn test_into_inner() {
        let m = Mutex::new(NonCopy(10));
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
        let m = Mutex::new(Foo(num_drops.clone()));
        assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        {
            let _inner = m.into_inner().unwrap();
            assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        }
        assert_eq!(num_drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_into_inner_poison() {
        let m = Arc::new(Mutex::new(NonCopy(10)));
        let m2 = m.clone();
        let _ = thread::spawn(move || {
            let _lock = m2.lock().unwrap();
            panic!("test panic in inner thread to poison mutex");
        })
        .join();

        assert!(m.is_poisoned());
        match Arc::try_unwrap(m).unwrap().into_inner() {
            Err(e) => assert_eq!(e.into_inner(), NonCopy(10)),
            Ok(x) => panic!("into_inner of poisoned Mutex is Ok: {x:?}"),
        }
    }

    #[test]
    fn test_get_mut() {
        let mut m = Mutex::new(NonCopy(10));
        *m.get_mut().unwrap() = NonCopy(20);
        assert_eq!(m.into_inner().unwrap(), NonCopy(20));
    }

    #[test]
    fn test_get_mut_poison() {
        let m = Arc::new(Mutex::new(NonCopy(10)));
        let m2 = m.clone();
        let _ = thread::spawn(move || {
            let _lock = m2.lock().unwrap();
            panic!("test panic in inner thread to poison mutex");
        })
        .join();

        assert!(m.is_poisoned());
        match Arc::try_unwrap(m).unwrap().get_mut() {
            Err(e) => assert_eq!(*e.into_inner(), NonCopy(10)),
            Ok(x) => panic!("get_mut of poisoned Mutex is Ok: {x:?}"),
        }
    }

    #[test]
    fn test_clear_poison() {
        let m = Arc::new(Mutex::new(NonCopy(10)));
        let m2 = m.clone();
        let _ = thread::spawn(move || {
            let _lock = m2.lock().unwrap();
            panic!("test panic in inner thread to poison mutex");
        })
        .join();

        assert!(m.is_poisoned());
        m.clear_poison();
        assert!(!m.is_poisoned());
        assert_eq!(*m.lock().unwrap(), NonCopy(10));
    }

    #[test]
    fn test_from() {
        let m = Mutex::from(NonCopy(10));
        assert_eq!(m.into_inner().unwrap(), NonCopy(10));
    }

    #[test]
    fn test_mutex_arc_condvar() {
        let packet = Packet(Arc::new((Mutex::new(false), Condvar::new())));
        let packet2 = Packet(packet.0.clone());
        let (tx, rx) = channel();
        let _t = thread::spawn(move || {
            // wait until parent gets in
            rx.recv().unwrap();
            let (lock, cvar) = &*packet2.0;
            let mut lock = lock.lock().unwrap();
            *lock = true;
            cvar.notify_one();
        });

        let (lock, cvar) = &*packet.0;
        let mut lock = lock.lock().unwrap();
        tx.send(()).unwrap();
        assert!(!*lock);
        while !*lock {
            lock = cvar.wait(lock).unwrap();
        }
    }

    #[test]
    fn test_arc_condvar_poison() {
        let packet = Packet(Arc::new((Mutex::new(1), Condvar::new())));
        let packet2 = Packet(packet.0.clone());
        let (tx, rx) = channel();

        let _t = thread::spawn(move || {
            rx.recv().unwrap();
            let (lock, cvar) = &*packet2.0;
            let _g = lock.lock().unwrap();
            cvar.notify_one();
            // Parent should fail when it wakes up.
            panic!();
        });

        let (lock, cvar) = &*packet.0;
        let mut lock = lock.lock().unwrap();
        tx.send(()).unwrap();
        while *lock == 1 {
            match cvar.wait(lock) {
                Ok(l) => {
                    lock = l;
                    assert_eq!(*lock, 1);
                }
                Err(..) => break,
            }
        }
    }

    #[test]
    fn test_mutex_arc_poison() {
        let arc = Arc::new(Mutex::new(1));
        assert!(!arc.is_poisoned());
        let arc2 = arc.clone();
        let _ = thread::spawn(move || {
            let lock = arc2.lock().unwrap();
            assert_eq!(*lock, 2);
        })
        .join();
        assert!(arc.lock().is_err());
        assert!(arc.is_poisoned());
    }

    #[test]
    fn test_mutex_arc_nested() {
        // Tests nested mutex and access
        // to underlying data.
        let arc = Arc::new(Mutex::new(1));
        let arc2 = Arc::new(Mutex::new(arc));
        let (tx, rx) = channel();
        let _t = thread::spawn(move || {
            let lock = arc2.lock().unwrap();
            let lock2 = lock.lock().unwrap();
            assert_eq!(*lock2, 1);
            tx.send(()).unwrap();
        });
        rx.recv().unwrap();
    }

    #[test]
    fn test_mutex_arc_access_in_unwind() {
        let arc = Arc::new(Mutex::new(1));
        let arc2 = arc.clone();
        let _ = thread::spawn(move || {
            struct Unwinder {
                i: Arc<Mutex<i32>>,
            }
            impl Drop for Unwinder {
                fn drop(&mut self) {
                    *self.i.lock().unwrap() += 1;
                }
            }
            let _u = Unwinder { i: arc2 };
            panic!();
        })
        .join();
        let lock = arc.lock().unwrap();
        assert_eq!(*lock, 2);
    }

    #[test]
    fn test_mutex_unsized() {
        let mutex: &Mutex<[i32]> = &Mutex::new([1, 2, 3]);
        {
            let b = &mut *mutex.lock().unwrap();
            b[0] = 4;
            b[2] = 5;
        }
        let comp: &[i32] = &[4, 2, 5];
        assert_eq!(&*mutex.lock().unwrap(), comp);
    }

    #[test]
    fn test_mutex_canceled() {
        use crate::sleep::sleep;
        use std::mem::drop;
        use std::time::Duration;

        let mutex1 = Arc::new(Mutex::new(0));
        let mutex2 = mutex1.clone();
        let mutex3 = mutex1.clone();
        let g = mutex1.lock().unwrap();

        let h1 = go!(move || {
            let mut g = mutex2.lock().unwrap();
            *g += 1;
        });

        let h2 = go!(move || {
            // let h1 enqueue
            sleep(Duration::from_millis(50));
            let mut g = mutex3.lock().unwrap();
            *g += 1;
        });

        // wait h1 and h2 enqueue
        sleep(Duration::from_millis(100));
        // cancel h1
        h1.coroutine().cancel();
        h1.join().unwrap_err();
        // release the mutex
        drop(g);
        h2.join().unwrap();
        let g = mutex1.lock().unwrap();
        assert_eq!(*g, 1);
    }

    #[test]
    fn test_mutex_canceled_by_other_wait() {
        use crate::sleep::sleep;
        use std::mem::drop;
        use std::time::Duration;

        let mutex1 = Arc::new(Mutex::new(0));
        let mutex2 = mutex1.clone();
        let mutex3 = mutex1.clone();
        let g = mutex1.lock().unwrap();

        let h1 = go!(move || {
            let mut g = mutex2.lock().unwrap();
            // test cancel when holding the mutex
            // this should not be poison
            sleep(Duration::from_secs(10000));
            *g += 1;
        });

        let h2 = go!(move || {
            // let h1 enqueue
            sleep(Duration::from_millis(50));
            let mut g = mutex3.lock().unwrap();
            *g += 1;
        });

        // wait h1 and h2 enqueue
        sleep(Duration::from_millis(100));
        // release the mutex, so that h1 go the lock
        drop(g);
        sleep(Duration::from_millis(50));
        // cancel h1 while h1 hold the lock
        h1.coroutine().cancel();
        h1.join().unwrap_err();
        h2.join().unwrap();
        let g = mutex1.lock().unwrap();
        assert_eq!(*g, 1);
    }

    // regression: releasing the lock must walk the chain of released
    // (canceled) waiters iteratively — a recursive unlock would overflow
    // the coroutine stack
    #[test]
    fn test_mutex_release_chain() {
        use crate::sleep::sleep;
        use crate::sync::mpsc::channel;
        use std::time::Duration;

        const N: usize = 500;

        let m = Arc::new(Mutex::new(()));
        let m_holder = m.clone();
        let (tx_held, rx_held) = channel();
        let (tx_go, rx_go) = channel();
        // the holder is a coroutine so the release-chain walk runs on a
        // 32KB coroutine stack; it holds the lock until the test tells it
        // to release, so the waiters can never acquire the lock
        let holder = go!(move || {
            let _g = m_holder.lock().unwrap();
            tx_held.send(()).unwrap(); // the lock is definitely held now
            rx_go.recv().unwrap(); // wait until all waiters are canceled
                                   // dropping the guard at scope end walks the chain
        });

        // wait until the holder acquired the lock before spawning the
        // waiters: otherwise a waiter could win the lock race, panic with
        // unreachable!() before being canceled, and poison the mutex
        rx_held.recv().unwrap();

        let mut handles = vec![];
        for _ in 0..N {
            let m = m.clone();
            handles.push(go!(move || {
                let _g = m.lock().unwrap(); // canceled while waiting
                unreachable!();
            }));
        }

        // let all waiters enqueue
        sleep(Duration::from_millis(100));
        // cancel all waiters while they are parked on the mutex: each one
        // registers a release action and then unwinds
        for h in &handles {
            h.coroutine().cancel();
        }
        for h in handles {
            assert!(h.join().is_err());
        }

        // the holder releases the lock and iteratively passes it down the
        // chain of released waiters
        tx_go.send(()).unwrap();
        holder.join().unwrap();
        // the mutex must be usable afterwards
        let g = m.lock().unwrap();
        drop(g);
    }
}
