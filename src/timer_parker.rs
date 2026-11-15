//! Wake-up and timed-wait machinery for the scheduler's timer thread.
//!
//! The timer thread waits for the next timeout deadline, and producers
//! ([`crate::timeout_list::TimerThread::add_timer`]/`del_timer`) wake it when
//! they queue work.
//!
//! On Unix the standard thread parker is used directly: it is a futex wait
//! with a nanosecond `CLOCK_MONOTONIC` deadline, which is precise enough for
//! coroutine timeouts.
//!
//! On Windows the standard parker is `WaitOnAddress`. Its timeout is a whole
//! number of milliseconds, rounded up, and the wait is quantized to the
//! system clock tick unless the process raises the timer resolution
//! (`timeBeginPeriod`). With the default resolution every short timeout
//! oversleeps by up to a tick (~15.6 ms), so a 1 ms `park_timeout` can take
//! 15 ms. The Windows implementation below instead waits on a waitable timer
//! created with `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` (Windows 10 1803+),
//! whose timeout is specified in 100 ns units and is not tied to the default
//! clock tick. A manual-reset notification event is waited on together with
//! the timer so producers can still interrupt the wait.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::sync::AtomicOption;

/// A parker built on the standard library's thread parking.
///
/// The timer thread publishes its `Thread` handle with [`arm`](Self::arm)
/// right before deciding to block. A producer [`notify`](Self::notify) takes
/// that handle and unparks the thread; a producer that raced with `arm` finds
/// the slot empty and does not wake it, so the timer thread re-checks its work
/// queues after arming and wakes itself if there is pending work.
pub(crate) struct StdParker {
    wakeup: AtomicOption<Arc<thread::Thread>>,
}

impl StdParker {
    pub(crate) fn new() -> Self {
        StdParker {
            wakeup: AtomicOption::none(),
        }
    }

    pub(crate) fn arm(&self, thread: &Arc<thread::Thread>) {
        self.wakeup.store(thread.clone());
    }

    pub(crate) fn notify(&self) {
        if let Some(t) = self.wakeup.take() {
            t.unpark();
        }
    }

    pub(crate) fn park(&self, timeout: Option<Duration>) {
        match timeout {
            Some(t) => thread::park_timeout(t),
            None => thread::park(),
        }
    }
}

#[cfg(not(windows))]
pub(crate) use StdParker as Parker;

#[cfg(windows)]
pub(crate) use self::windows::Parker;

#[cfg(windows)]
mod windows {
    use super::StdParker;
    use std::io;
    use std::ptr;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        CancelWaitableTimer, CreateEventW, CreateWaitableTimerExW, CreateWaitableTimerW,
        ResetEvent, SetEvent, SetWaitableTimer, WaitForMultipleObjects,
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE, TIMER_ALL_ACCESS,
    };

    /// Index of the notification event in the wait handle array: a
    /// notification is checked before the timer, so a producer's wake wins a
    /// tie.
    const EVENT_INDEX: u32 = 0;
    /// Index of the waitable timer in the wait handle array.
    const TIMER_INDEX: u32 = 1;

    /// A waitable timer paired with the event that interrupts its wait.
    ///
    /// The timer is auto-reset (the default without
    /// `CREATE_WAITABLE_TIMER_MANUAL_RESET`), so one expiry produces at most
    /// one signal and a wait consumes it.
    struct WaitableTimer {
        timer: HANDLE,
        event: HANDLE,
    }

    impl WaitableTimer {
        fn new() -> Option<WaitableTimer> {
            unsafe {
                // manual-reset event, initially unsignaled
                let event = CreateEventW(ptr::null(), 1, 0, ptr::null());
                if event.is_null() {
                    return None;
                }

                let mut timer = CreateWaitableTimerExW(
                    ptr::null(),
                    ptr::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS,
                );
                if timer.is_null() {
                    // Windows before 10 1803 does not know the high
                    // resolution flag. A regular waitable timer still gives
                    // the notification-event protocol, but its timeout is
                    // quantized like the standard parker.
                    timer = CreateWaitableTimerW(ptr::null(), 0, ptr::null());
                }
                if timer.is_null() {
                    let _ = CloseHandle(event);
                    return None;
                }

                Some(WaitableTimer { timer, event })
            }
        }

        /// Wait until notified or until `timeout` elapses (`None` waits
        /// forever).
        fn wait(&self, timeout: Option<Duration>) {
            unsafe {
                match timeout {
                    Some(dur) => {
                        // `SetWaitableTimer` takes a relative due time as a
                        // negative count of 100 ns units. Round up so the
                        // timer never fires before the requested deadline;
                        // a zero relative time is not used because the
                        // negative sign is what selects the relative form.
                        let units = dur.as_nanos().div_ceil(100);
                        let due = -((units.min(i64::MAX as u128) as i64).max(1));
                        if SetWaitableTimer(self.timer, &due, 0, None, ptr::null(), 0) == 0 {
                            error!("SetWaitableTimer failed: {}", io::Error::last_os_error());
                            // Do not wait on a broken timer: sleep the
                            // requested time with the standard clock, then
                            // let the caller recompute the deadline instead
                            // of blocking until a notification arrives.
                            thread::sleep(dur);
                            return;
                        }
                    }
                    None => {
                        let _ = CancelWaitableTimer(self.timer);
                    }
                }

                let handles = [self.event, self.timer];
                let r = WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE);

                // Clear the notification before the caller re-checks its
                // work queues. `notify` only needs to interrupt this wait;
                // the work it queued is picked up by the re-check, so a
                // notification that coalesces with a timer expiry is not
                // lost.
                let _ = ResetEvent(self.event);

                if r == WAIT_FAILED {
                    error!(
                        "WaitForMultipleObjects failed: {}",
                        io::Error::last_os_error()
                    );
                    // Avoid a hot spin if the handles have gone bad.
                    thread::sleep(Duration::from_millis(1));
                    return;
                }
                debug_assert!(
                    r == WAIT_OBJECT_0 + EVENT_INDEX || r == WAIT_OBJECT_0 + TIMER_INDEX,
                    "unexpected wait result {r}"
                );
            }
        }
    }

    impl Drop for WaitableTimer {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.timer);
                let _ = CloseHandle(self.event);
            }
        }
    }

    /// Parker for the timer thread.
    pub(crate) struct Parker {
        backend: Backend,
    }

    enum Backend {
        Timer(WaitableTimer),
        /// Used when the waitable timer object cannot be created.
        Std(StdParker),
    }

    // SAFETY: the handles are kernel object handles; the timer and the event
    // are designed for concurrent use across threads, and the kernel
    // serializes the operations on them. `StdParker` is already `Send + Sync`.
    unsafe impl Send for Parker {}
    unsafe impl Sync for Parker {}

    impl Parker {
        pub(crate) fn new() -> Self {
            match WaitableTimer::new() {
                Some(timer) => Parker {
                    backend: Backend::Timer(timer),
                },
                None => {
                    warn!(
                        "can't create a high resolution waitable timer, \
                         falling back to the standard thread parker"
                    );
                    Parker {
                        backend: Backend::Std(StdParker::new()),
                    }
                }
            }
        }

        pub(crate) fn arm(&self, thread: &Arc<thread::Thread>) {
            if let Backend::Std(ref std) = self.backend {
                std.arm(thread);
            }
        }

        pub(crate) fn notify(&self) {
            match self.backend {
                Backend::Timer(ref t) => {
                    // A failure can only mean a bad handle; there is nothing
                    // useful to do besides reporting it.
                    if unsafe { SetEvent(t.event) } == 0 {
                        error!("SetEvent failed: {}", io::Error::last_os_error());
                    }
                }
                Backend::Std(ref std) => std.notify(),
            }
        }

        pub(crate) fn park(&self, timeout: Option<Duration>) {
            match self.backend {
                Backend::Timer(ref t) => t.wait(timeout),
                Backend::Std(ref std) => std.park(timeout),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    /// A notification from another thread must wake an indefinite park.
    #[test]
    fn notify_wakes_park() {
        let parker = Arc::new(Parker::new());
        let notifier = parker.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            notifier.notify();
        });

        parker.arm(&Arc::new(std::thread::current()));
        let start = Instant::now();
        parker.park(None);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "park was not woken by notify"
        );
        handle.join().unwrap();
    }

    /// A timed park must return on its own, at least once, and must not
    /// require a notification.
    #[test]
    fn timed_park_returns() {
        let parker = Parker::new();
        parker.arm(&Arc::new(std::thread::current()));
        let start = Instant::now();
        parker.park(Some(Duration::from_millis(1)));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed park did not return"
        );
    }
}
