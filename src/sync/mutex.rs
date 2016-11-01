//! compatable with std::sync::mutex except for both thread and coroutine
//! please ref the doc from std::sync::mutex
use std::fmt;
use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{TryLockError, TryLockResult, LockResult};
use sync::poison;
// use sync::Blocker;
// use queue::mpsc_list;

pub struct Mutex<T: ?Sized> {
    poison: poison::Flag,
    // track how many blockers are waiting on the mutex
    cnt: AtomicUsize,
    // the waiting blocker list
    // to_wake: mpsc_list::Queue<Blocker>,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

pub struct MutexGuard<'a, T: ?Sized + 'a> {
    // funny underscores due to how Deref/DerefMut currently work (they
    // disregard field privacy).
    __lock: &'a Mutex<T>,
    __poison: poison::Guard,
}

// impl<'a, T: ?Sized> !Send for MutexGuard<'a, T> {}

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    pub fn new(t: T) -> Mutex<T> {
        Mutex {
            // to_wake: mpsc_list::Queue::new(),
            cnt: AtomicUsize::new(0),
            poison: poison::Flag::new(),
            data: UnsafeCell::new(t),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> LockResult<MutexGuard<T>> {
        // self.lock();
        MutexGuard::new(self)
    }

    pub fn try_lock(&self) -> TryLockResult<MutexGuard<T>> {
        if self.cnt.load(Ordering::Relaxed) == 0 {
            Ok(try!(MutexGuard::new(self)))
        } else {
            Err(TryLockError::WouldBlock)
        }
    }

    fn unlock(&self) {}

    #[inline]
    pub fn is_poisoned(&self) -> bool {
        self.poison.get()
    }

    pub fn into_inner(self) -> LockResult<T>
        where T: Sized
    {
        let data = unsafe { self.data.into_inner() };
        poison::map_result(self.poison.borrow(), |_| data)
    }

    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        // We know statically that there are no other references to `self`, so
        // there's no need to lock the inner lock.
        let data = unsafe { &mut *self.data.get() };
        poison::map_result(self.poison.borrow(), |_| data)
    }
}

impl<T: ?Sized + Default> Default for Mutex<T> {
    fn default() -> Mutex<T> {
        Mutex::new(Default::default())
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
        poison::map_result(lock.poison.borrow(), |guard| {
            MutexGuard {
                __lock: lock,
                __poison: guard,
            }
        })
    }
}

impl<'mutex, T: ?Sized> Deref for MutexGuard<'mutex, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.__lock.data.get() }
    }
}

impl<'mutex, T: ?Sized> DerefMut for MutexGuard<'mutex, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.__lock.data.get() }
    }
}

impl<'a, T: ?Sized> Drop for MutexGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        self.__lock.poison.done(&self.__poison);
        self.__lock.unlock();
    }
}

// pub fn guard_lock<'a, T: ?Sized>(guard: &MutexGuard<'a, T>) -> &'a sys::Mutex {
//     &guard.__lock.inner
// }
//
// pub fn guard_poison<'a, T: ?Sized>(guard: &MutexGuard<'a, T>) -> &'a poison::Flag {
//     &guard.__lock.poison
// }

#[cfg(test)]
mod tests {
    // use std::sync::{Arc, Condvar};
    use std::thread;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use coroutine;
    use sync::mpsc::channel;
    use super::*;

    // struct Packet<T>(Arc<(Mutex<T>, Condvar)>);
    // unsafe impl<T: Send> Send for Packet<T> {}
    // unsafe impl<T> Sync for Packet<T> {}

    #[derive(Eq, PartialEq, Debug)]
    struct NonCopy(i32);

    #[test]
    fn smoke() {
        let m = Mutex::new(());
        drop(m.lock().unwrap());
        drop(m.lock().unwrap());
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
            coroutine::spawn(move || {
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
            Ok(x) => panic!("into_inner of poisoned Mutex is Ok: {:?}", x),
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
            Ok(x) => panic!("get_mut of poisoned Mutex is Ok: {:?}", x),
        }
    }

    // #[test]
    // fn test_mutex_arc_condvar() {
    //     let packet = Packet(Arc::new((Mutex::new(false), Condvar::new())));
    //     let packet2 = Packet(packet.0.clone());
    //     let (tx, rx) = channel();
    //     let _t = thread::spawn(move || {
    //         // wait until parent gets in
    //         rx.recv().unwrap();
    //         let &(ref lock, ref cvar) = &*packet2.0;
    //         let mut lock = lock.lock().unwrap();
    //         *lock = true;
    //         cvar.notify_one();
    //     });
    //
    //     let &(ref lock, ref cvar) = &*packet.0;
    //     let mut lock = lock.lock().unwrap();
    //     tx.send(()).unwrap();
    //     assert!(!*lock);
    //     while !*lock {
    //         lock = cvar.wait(lock).unwrap();
    //     }
    // }

    // #[test]
    // fn test_arc_condvar_poison() {
    //     let packet = Packet(Arc::new((Mutex::new(1), Condvar::new())));
    //     let packet2 = Packet(packet.0.clone());
    //     let (tx, rx) = channel();
    //
    //     let _t = thread::spawn(move || -> () {
    //         rx.recv().unwrap();
    //         let &(ref lock, ref cvar) = &*packet2.0;
    //         let _g = lock.lock().unwrap();
    //         cvar.notify_one();
    //         // Parent should fail when it wakes up.
    //         panic!();
    //     });
    //
    //     let &(ref lock, ref cvar) = &*packet.0;
    //     let mut lock = lock.lock().unwrap();
    //     tx.send(()).unwrap();
    //     while *lock == 1 {
    //         match cvar.wait(lock) {
    //             Ok(l) => {
    //                 lock = l;
    //                 assert_eq!(*lock, 1);
    //             }
    //             Err(..) => break,
    //         }
    //     }
    // }

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
        // Tests nested mutexes and access
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
        let _ = thread::spawn(move || -> () {
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
}
