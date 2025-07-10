//! a lightweight blocker compared with `may::sync::Blocker`
//! this is not reusable, only for one time use
//! and no timeout blocking support

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{blocking::ThreadPark, AtomicOption};
use crate::coroutine_impl::{
    co_cancel_data, is_coroutine, run_coroutine, CoroutineImpl, EventSource,
};
use crate::park::ParkError;
use crate::scheduler::get_scheduler;
use crate::yield_now::{get_co_para, yield_with};

#[derive(Debug)]
pub struct Park {
    // the coroutine that waiting for this park instance
    wait_co: Arc<AtomicOption<CoroutineImpl>>,
    // flag if parked
    state: AtomicBool,
    // the container of the park to hold resource in kernel
    container: UnsafeCell<Option<Arc<Blocker>>>,
}

// this is the park resource type (spmc style)
impl Park {
    pub fn new() -> Self {
        Park {
            wait_co: Arc::new(AtomicOption::none()),
            state: AtomicBool::new(false),
            container: UnsafeCell::new(None),
        }
    }

    /// park current coroutine
    /// if cancellation detected, return Err(ParkError::Canceled)
    pub fn park(&self, container: Arc<Blocker>) -> Result<(), ParkError> {
        if self.state.load(Ordering::Acquire) {
            return Ok(());
        }
        unsafe { *self.container.get() = Some(container) };
        yield_with(self);

        if let Some(err) = get_co_para() {
            match err.kind() {
                std::io::ErrorKind::Other => return Err(ParkError::Canceled),
                std::io::ErrorKind::TimedOut => return Err(ParkError::Timeout),
                _ => unreachable!("unexpected return error kind"),
            }
        }

        Ok(())
    }

    // unpark the underlying coroutine if any, push to the ready task queue
    #[inline]
    pub fn unpark(&self) {
        self.state.store(true, Ordering::Release);
        if let Some(co) = self.wait_co.take() {
            get_scheduler().schedule(co);
        }
    }
}

impl EventSource for Park {
    // register the coroutine to the park
    fn subscribe(&mut self, co: CoroutineImpl) {
        let cancel = co_cancel_data(&co);
        // delay drop the container here to hold the resource
        let _container = self.container.get_mut().take().unwrap();
        // register the coroutine
        self.wait_co.store(co);
        // re-check the state, only clear once after resume
        if self.state.load(Ordering::Acquire) {
            // fast check first
            if !self.wait_co.is_none() {
                if let Some(co) = self.wait_co.take() {
                    run_coroutine(co);
                }
            }
            return;
        }

        // register the cancel data
        cancel.set_co(self.wait_co.clone());
        // re-check the cancel status
        if cancel.is_canceled() {
            unsafe { cancel.cancel() };
        }
    }
}

#[derive(Debug)]
pub enum Blocker {
    Coroutine(Park),
    Thread(ThreadPark),
}

impl Blocker {
    /// create a new fast blocker
    pub fn new() -> Arc<Self> {
        let blocker = if is_coroutine() {
            Blocker::Coroutine(Park::new())
        } else {
            Blocker::Thread(ThreadPark::new())
        };
        Arc::new(blocker)
    }

    #[inline]
    pub fn park(self: &Arc<Self>) -> Result<(), ParkError> {
        match self.as_ref() {
            Blocker::Coroutine(ref co) => co.park(self.clone()),
            Blocker::Thread(ref t) => t.park_timeout(None),
        }
    }

    #[inline]
    pub fn unpark(&self) {
        match self {
            Blocker::Coroutine(ref co) => co.unpark(),
            Blocker::Thread(ref t) => t.unpark(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;
    use crate::coroutine_impl::is_coroutine;

    #[test]
    fn test_park_new() {
        let park = Park::new();
        // Verify park is created with initial state
        assert!(!park.state.load(Ordering::Acquire));
    }

    #[test]
    fn test_park_unpark() {
        let park = Park::new();
        
        // Test unpark sets state to true
        park.unpark();
        assert!(park.state.load(Ordering::Acquire));
        
        // Test multiple unparks are safe
        park.unpark();
        assert!(park.state.load(Ordering::Acquire));
    }

    #[test]
    fn test_blocker_new() {
        // Test that blocker can be created
        let blocker = Blocker::new();
        
        // Verify it's created successfully
        match blocker.as_ref() {
            Blocker::Thread(_) => {
                // Should be thread blocker when not in coroutine context
                assert!(!is_coroutine());
            }
            Blocker::Coroutine(_) => {
                // Should be coroutine blocker when in coroutine context
                assert!(is_coroutine());
            }
        }
    }

    #[test]
    fn test_blocker_unpark() {
        let blocker = Blocker::new();
        
        // Test unpark doesn't panic
        blocker.unpark();
        
        // Test multiple unparks are safe
        blocker.unpark();
        blocker.unpark();
    }

    #[test]
    fn test_park_unpark_before_park() {
        let park = Park::new();
        
        // Unpark before any park call
        park.unpark();
        
        // State should be true
        assert!(park.state.load(Ordering::Acquire));
    }

    #[test]
    fn test_park_state_transitions() {
        let park = Park::new();
        
        // Initial state should be false
        assert!(!park.state.load(Ordering::Acquire));
        
        // After unpark, state should be true
        park.unpark();
        assert!(park.state.load(Ordering::Acquire));
        
        // State should remain true after multiple unparks
        park.unpark();
        assert!(park.state.load(Ordering::Acquire));
    }

    #[test]
    fn test_blocker_thread_park_unpark() {
        let blocker = Arc::new(Blocker::new());
        let blocker_clone = blocker.clone();
        
        // Test thread-based park/unpark
        let handle = thread::spawn(move || {
            // Small delay to ensure park is called first
            thread::sleep(Duration::from_millis(10));
            blocker_clone.unpark();
        });
        
        // This should complete when unpark is called
        let result = blocker.park();
        handle.join().unwrap();
        
        // Should not return an error for thread-based blocking
        assert!(result.is_ok());
    }

    #[test]
    fn test_blocker_multiple_unparks() {
        let blocker = Blocker::new();
        
        // Test multiple unparks don't cause issues
        for _ in 0..5 {
            blocker.unpark();
        }
        
        // Park should return immediately since already unparked
        let result = blocker.park();
        assert!(result.is_ok());
    }

    #[test]
    fn test_park_wait_co_none_initially() {
        let park = Park::new();
        
        // wait_co should be None initially
        assert!(park.wait_co.is_none());
    }

    #[test]
    fn test_park_container_initially_none() {
        let park = Park::new();
        
        // Container should be None initially
        unsafe {
            assert!((*park.container.get()).is_none());
        }
    }

    #[test]
    fn test_blocker_debug_format() {
        let blocker = Blocker::new();
        let debug_str = format!("{:?}", blocker);
        
        // Should contain either "Thread" or "Coroutine" based on context
        assert!(debug_str.contains("Thread") || debug_str.contains("Coroutine"));
    }

    #[test]
    fn test_park_debug_format() {
        let park = Park::new();
        let debug_str = format!("{:?}", park);
        
        // Should contain "Park" in debug output
        assert!(debug_str.contains("Park"));
    }
}
