//! compatible with std::sync::rwlock except for both thread and coroutine
//! please ref the doc from std::sync::rwlock
use std::cell::UnsafeCell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};

use crate::cancel::trigger_cancel_panic;
use may_queue::mpsc_list;
use crate::park::ParkError;

use super::blocking::SyncBlocker;
use super::mutex::{self, Mutex};
use super::poison;

/// A reader-writer lock
///
/// The priority policy of the lock is that readers have weak priority
pub struct RwLock<T: ?Sized> {
    // below two variables consist a global mutex
    // we need to deal with the cancel logic differently
    // the waiting blocker list
    to_wake: mpsc_list::Queue<Arc<SyncBlocker>>,
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

#[must_use]
pub struct RwLockReadGuard<'a, T: ?Sized + 'a> {
    __lock: &'a RwLock<T>,
}

// impl<'a, T: ?Sized> !marker::Send for RwLockReadGuard<'a, T> {}

#[must_use]
pub struct RwLockWriteGuard<'a, T: ?Sized + 'a> {
    __lock: &'a RwLock<T>,
    __poison: poison::Guard,
}

// impl<'a, T: ?Sized> !marker::Send for RwLockWriteGuard<'a, T> {}

impl<T> RwLock<T> {
    pub fn new(t: T) -> RwLock<T> {
        RwLock {
            to_wake: mpsc_list::Queue::new(),
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
            Err(TryLockError::Poisoned(_)) => return Err(ParkError::Timeout),
        }

        let cur = SyncBlocker::current();
        // register blocker first
        self.to_wake.push(cur.clone());
        // inc the cnt, if it's the first grab, unpark the first waiter
        if self.cnt.fetch_add(1, Ordering::SeqCst) == 0 {
            self.to_wake
                .pop()
                .map_or_else(|| panic!("got null blocker!"), |w| self.unpark_one(&w));
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
        if self.cnt.load(Ordering::SeqCst) == 0 {
            match self
                .cnt
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => Ok(()),
                Err(_) => {
                    if self.poison.get() {
                        Err(TryLockError::Poisoned(PoisonError::new(())))
                    } else {
                        Err(TryLockError::WouldBlock)
                    }
                }
            }
        } else {
            Err(TryLockError::WouldBlock)
        }
    }

    fn unlock(&self) {
        if self.cnt.fetch_sub(1, Ordering::SeqCst) > 1 {
            self.to_wake
                .pop()
                .map_or_else(|| panic!("got null blocker!"), |w| self.unpark_one(&w));
        }
    }

    fn unpark_one(&self, w: &SyncBlocker) {
        w.unpark();
        if w.take_release() {
            self.unlock();
        }
    }

    pub fn read(&self) -> LockResult<RwLockReadGuard<T>> {
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

    pub fn try_read(&self) -> TryLockResult<RwLockReadGuard<T>> {
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

        let g = RwLockReadGuard::new(self)?;
        // finally we add rlock
        *r += 1;
        Ok(g)
    }

    fn read_unlock(&self) {
        let mut r = self.rlock.lock().expect("rwlock read_unlock");
        *r -= 1;
        if *r == 0 {
            self.unlock();
        }
    }

    pub fn write(&self) -> LockResult<RwLockWriteGuard<T>> {
        if let Err(ParkError::Canceled) = self.lock() {
            // now we can safely go with the cancel panic
            trigger_cancel_panic();
        }
        RwLockWriteGuard::new(self)
    }

    pub fn try_write(&self) -> TryLockResult<RwLockWriteGuard<T>> {
        if let Err(TryLockError::WouldBlock) = self.try_lock() {
            return Err(TryLockError::WouldBlock);
        }
        Ok(RwLockWriteGuard::new(self)?)
    }

    fn write_unlock(&self) {
        self.unlock();
    }

    pub fn is_poisoned(&self) -> bool {
        self.poison.get()
    }

    pub fn into_inner(self) -> LockResult<T>
    where
        T: Sized,
    {
        // We know statically that there are no outstanding references to
        // `self` so there's no need to lock the inner lock.
        let data = self.data.into_inner();
        poison::map_result(self.poison.borrow(), |_| data)
    }

    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        // We know statically that there are no other references to `self`, so
        // there's no need to lock the inner lock.
        let data = unsafe { &mut *self.data.get() };
        poison::map_result(self.poison.borrow(), |_| data)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.try_read() {
            Ok(guard) => write!(f, "RwLock {{ data: {:?} }}", &*guard),
            Err(TryLockError::Poisoned(err)) => {
                write!(f, "RwLock {{ data: Poisoned({:?}) }}", &**err.get_ref())
            }
            Err(TryLockError::WouldBlock) => write!(f, "RwLock {{ <locked> }}"),
        }
    }
}

impl<T: Default> Default for RwLock<T> {
    /// Creates a new `RwLock<T>`, with the `Default` value for T.
    fn default() -> RwLock<T> {
        RwLock::new(Default::default())
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

impl<'a, T: fmt::Debug> fmt::Debug for RwLockReadGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("RwLockReadGuard")
            .field("lock", &self.__lock)
            .finish()
    }
}

impl<'a, T: fmt::Debug> fmt::Debug for RwLockWriteGuard<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("RwLockWriteGuard")
            .field("lock", &self.__lock)
            .finish()
    }
}

impl<'rwlock, T: ?Sized> Deref for RwLockReadGuard<'rwlock, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.__lock.data.get() }
    }
}

impl<'rwlock, T: ?Sized> Deref for RwLockWriteGuard<'rwlock, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.__lock.data.get() }
    }
}

impl<'rwlock, T: ?Sized> DerefMut for RwLockWriteGuard<'rwlock, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.__lock.data.get() }
    }
}

impl<'a, T: ?Sized> Drop for RwLockReadGuard<'a, T> {
    fn drop(&mut self) {
        self.__lock.read_unlock();
    }
}

impl<'a, T: ?Sized> Drop for RwLockWriteGuard<'a, T> {
    fn drop(&mut self) {
        self.__lock.poison.done(&self.__poison);
        self.__lock.write_unlock();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, TryLockError};
    use std::thread;
    use crate::sync::mpsc::channel;
    use crate::sync::{Condvar, Mutex, RwLock};

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
        let _ = thread::spawn(move || -> () {
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
            Ok(_) => assert!(
                false,
                "try_write should not succeed while read_guard is in scope"
            ),
            Err(_) => assert!(false, "unexpected error"),
        }

        drop(read_guard);
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
            Ok(x) => panic!("into_inner of poisoned RwLock is Ok: {:?}", x),
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
            Ok(x) => panic!("get_mut of poisoned RwLock is Ok: {:?}", x),
        }
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
                let &(ref lock, ref cond) = &*sync;
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
        if cancel_id == N + 2 {
            cancel_id = 1;
        }
        // println!("cancel id = {}", cancel_id);
        unsafe { vec[cancel_id - 1].coroutine().cancel() };

        // let all coroutine to continue
        let &(ref lock, ref cond) = &*sync;
        for _ in 1..N {
            let mut cnt = lock.lock().unwrap();
            *cnt = id;
            cond.notify_all();
            drop(cnt);
            id = rx.recv().unwrap_or(0);
            // println!("recv id = {:?}", id);
        }

        assert_eq!(rx.try_recv().is_err(), true);
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
        unsafe { h.coroutine().cancel() };
        h.join().unwrap_err();

        // release the write lock, so that other thread can got the lock
        drop(wlock);
        let a = rx.recv().unwrap();
        assert_eq!(a, 10);
        assert_eq!(rx.try_recv().is_err(), true);
    }
}
