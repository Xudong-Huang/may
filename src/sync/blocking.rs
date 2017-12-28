use std::sync::Arc;
use std::time::Duration;
use std::sync::{Condvar, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use park::{Park, ParkError};
use coroutine_impl::is_coroutine;

#[derive(Debug)]
pub struct ThreadPark {
    lock: Mutex<bool>,
    cvar: Condvar,
}

impl ThreadPark {
    fn new() -> Self {
        ThreadPark {
            lock: Mutex::new(false),
            cvar: Condvar::new(),
        }
    }

    fn park_timeout(&self, dur: Option<Duration>) -> Result<(), ParkError> {
        let mut result = Ok(());
        let mut guard = self.lock.lock().unwrap();
        while !*guard && result.is_ok() {
            let g = match dur {
                None => self.cvar.wait(guard).unwrap(),
                Some(t) => {
                    let (g, t) = self.cvar.wait_timeout(guard, t).unwrap();
                    if t.timed_out() {
                        result = Err(ParkError::Timeout);
                    }
                    g
                }
            };
            guard = g;
        }
        // must clear the status
        *guard = false;
        result
    }

    fn unpark(&self) {
        let mut guard = self.lock.lock().unwrap();
        if !*guard {
            *guard = true;
            self.cvar.notify_one();
        }
    }
}

#[derive(Debug)]
pub enum Parker {
    Coroutine(Park),
    Thread(ThreadPark),
}

#[derive(Debug)]
pub struct Blocker {
    parker: Parker,
}

impl Blocker {
    /// create a new blocker
    pub fn new(ignore_cancel: bool) -> Self {
        let parker = if is_coroutine() {
            let park = Park::new();
            park.ignore_cancel(ignore_cancel);
            Parker::Coroutine(park)
        } else {
            let park = ThreadPark::new();
            Parker::Thread(park)
        };

        Blocker { parker: parker }
    }

    /// get the internal shared blocker
    pub fn current() -> Arc<Self> {
        Arc::new(Self::new(false))
    }

    #[inline]
    pub fn park(&self, timeout: Option<Duration>) -> Result<(), ParkError> {
        match self.parker {
            Parker::Coroutine(ref co) => co.park_timeout(timeout),
            Parker::Thread(ref t) => t.park_timeout(timeout),
        }
    }

    #[inline]
    pub fn unpark(&self) {
        match self.parker {
            Parker::Coroutine(ref co) => co.unpark(),
            Parker::Thread(ref t) => t.unpark(),
        }
    }
}

/// a blocker type with async release support
/// the blocker would ignore the cancel
/// need to deal with it in custom logic
#[derive(Debug)]
pub struct SyncBlocker {
    // flag to tell unparked
    unparked: AtomicBool,
    // used to register release action
    release: AtomicBool,
    blocker: Blocker,
}

impl SyncBlocker {
    pub fn current() -> Arc<Self> {
        let blocker = Blocker::new(true);

        Arc::new(SyncBlocker {
            unparked: AtomicBool::new(false),
            release: AtomicBool::new(false),
            blocker: blocker,
        })
    }

    #[inline]
    pub fn is_unparked(&self) -> bool {
        self.unparked.load(Ordering::Acquire)
    }
    // set the Flag for the release action
    #[inline]
    pub fn set_release(&self) {
        self.release.store(true, Ordering::Release);
    }

    // take the release Flag
    #[inline]
    pub fn take_release(&self) -> bool {
        self.release.swap(false, Ordering::Acquire)
    }

    #[inline]
    pub fn park(&self, timeout: Option<Duration>) -> Result<(), ParkError> {
        self.blocker.park(timeout)
    }

    #[inline]
    pub fn unpark(&self) {
        self.blocker.unpark();
        self.unparked.store(true, Ordering::Release);
    }
}
