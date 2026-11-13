#[cfg(any(target_os = "linux", target_os = "android"))]
#[path = "epoll.rs"]
mod select;

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "ios",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd"
))]
#[path = "kqueue.rs"]
mod select;

#[cfg(feature = "io_cancel")]
pub mod cancel;
pub mod co_io;
pub mod net;
pub mod wait_io;

use std::ops::Deref;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::{fmt, io};

use crate::coroutine_impl::{run_coroutine, CoroutineImpl};
use crate::io::thread::ASSOCIATED_IO_RET;
use crate::likely::likely;
use crate::scheduler::get_scheduler;
use crate::sync::AtomicOption;
#[cfg(feature = "io_timeout")]
use crate::timeout_list::{TimeOutList, TimeoutHandle};
use crate::yield_now::get_co_para;
#[cfg(feature = "io_timeout")]
use crate::yield_now::set_co_para;

pub(crate) use self::select::{Selector, SysEvent};

#[inline]
pub fn add_socket<T: AsRawFd + ?Sized>(t: &T) -> io::Result<IoData> {
    get_scheduler().get_selector().add_fd(IoData::new(t))
}

#[inline]
pub fn mod_socket(io: &IoData, is_read: bool) -> io::Result<()> {
    get_scheduler().get_selector().mod_fd(io, is_read)
}

#[inline]
fn del_socket(io: &IoData) {
    // transfer the io to the selector
    get_scheduler().get_selector().del_fd(io);
}

// deal with the io result
#[inline]
fn co_io_result(is_coroutine: bool) -> io::Result<()> {
    if likely(is_coroutine) {
        match get_co_para() {
            None => Ok(()),
            Some(err) => Err(err),
        }
    } else {
        let err = ASSOCIATED_IO_RET.with(|io_ret| io_ret.take());
        match err {
            None => Ok(()),
            Some(err) => Err(*err),
        }
    }
}

#[inline]
fn from_nix_error(err: nix::Error) -> ::std::io::Error {
    std::io::Error::from_raw_os_error(err as i32)
}

#[cfg(feature = "io_timeout")]
fn timeout_handler(data: TimerData) {
    if data.event_data.is_null() {
        return;
    }

    // the event data is shared through `Arc`; the fields are
    // interior-mutable, so a shared reference is all this needs
    let event_data = unsafe { &*data.event_data };
    // remove the event timer
    event_data.timer.take();

    // get and check the coroutine
    let mut co = match event_data.co.take() {
        Some(co) => co,
        None => return,
    };

    set_co_para(&mut co, io::Error::new(io::ErrorKind::TimedOut, "timeout"));

    // resume the coroutine with timeout error
    run_coroutine(co);
}

// the timeout data
#[cfg(feature = "io_timeout")]
pub struct TimerData {
    event_data: *mut EventData,
}

#[cfg(feature = "io_timeout")]
unsafe impl Send for TimerData {}

#[cfg(feature = "io_timeout")]
pub type TimerList = TimeOutList<TimerData>;
#[cfg(feature = "io_timeout")]
pub type TimerHandle = TimeoutHandle<TimerData>;

// event associated io data, must be construct in
// each file handle, the epoll event.data would point to it
pub struct EventData {
    pub fd: RawFd,
    pub io_flag: AtomicUsize,
    #[cfg(feature = "io_timeout")]
    pub timer: AtomicOption<TimerHandle>,
    pub co: AtomicOption<CoroutineImpl>,
}

unsafe impl Send for EventData {}
unsafe impl Sync for EventData {}

impl EventData {
    pub fn new(fd: RawFd) -> EventData {
        EventData {
            fd,
            io_flag: AtomicUsize::new(0),
            #[cfg(feature = "io_timeout")]
            timer: AtomicOption::none(),
            co: AtomicOption::none(),
        }
    }

    #[cfg(feature = "io_timeout")]
    pub fn timer_data(&self) -> TimerData {
        TimerData {
            event_data: self as *const _ as *mut _,
        }
    }

    /// Disarm the timer registered for this io request, if any.
    ///
    /// The `event_data` pointer in the timer data is cleared first: the entry
    /// may be the last node of the timer list, in which case `remove` cannot
    /// unlink it, and the timeout handler relies on the null pointer to ignore
    /// the stale entry. Removing the entry is what keeps an aborted wait (e.g.
    /// a cancelled io) from firing later: without it the timer stays armed and
    /// can take the coroutine of a later wait that reuses the same fd, and
    /// then dereference this `EventData` after it is freed.
    #[cfg(feature = "io_timeout")]
    #[inline]
    pub fn take_timer(&self) {
        let h = match self.timer.take() {
            Some(h) => h,
            None => return,
        };
        // tell the timer handler not to cancel the io
        // it's not always true that you can really remove the timer entry
        h.with_mut_data(|value| value.data.event_data = std::ptr::null_mut());
        // a no-op when the timer already fired: its value is gone, so `remove`
        // returns `None` without touching the node
        let _ = h.remove();
    }

    #[inline]
    pub fn schedule(&self) {
        let co = match self.co.take() {
            Some(co) => co,
            None => return, // it's already take by selector
        };

        // it's safe to remove the timer since we are running the timer_list in the same thread
        #[cfg(feature = "io_timeout")]
        self.take_timer();

        // schedule the coroutine
        get_scheduler().schedule(co);
    }

    /// used by local re-schedule that in `subscribe`
    #[inline]
    pub fn fast_schedule(&self) {
        let co = match self.co.take() {
            Some(co) => co,
            None => return, // it's already take by selector
        };

        // it's safe to remove the timer since we are running the timer_list in the same thread
        #[cfg(feature = "io_timeout")]
        self.take_timer();

        // run the coroutine
        run_coroutine(co);
    }
}

// each file associated data
pub struct IoData {
    event_data: Arc<EventData>,
    // whether the fd has been successfully added to the selector
    registered: bool,
}

impl IoData {
    pub fn new<T: AsRawFd + ?Sized>(t: &T) -> Self {
        let fd = t.as_raw_fd();
        let event_data = Arc::new(EventData::new(fd));
        IoData {
            event_data,
            registered: false,
        }
    }

    /// Mark this io data as successfully registered with the selector.
    ///
    /// Only a registered `IoData` may run the deregistration in `Drop`: after
    /// a failed registration attempt there is nothing to remove, and the DEL
    /// would remove the registration of another owner of the same fd. That is
    /// exactly what happens on `EEXIST`, returned when a second `CoIo` wraps
    /// an fd that is already registered.
    #[inline]
    fn mark_registered(&mut self) {
        self.registered = true;
    }

    // clear the io flag
    #[inline]
    pub fn reset(&self) -> usize {
        self.event_data.io_flag.swap(0, Ordering::AcqRel)
    }
}

impl Deref for IoData {
    type Target = Arc<EventData>;

    fn deref(&self) -> &Arc<EventData> {
        &self.event_data
    }
}

impl fmt::Debug for IoData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "IoData = {{ ... }}")
    }
}

impl Drop for IoData {
    fn drop(&mut self) {
        // an fd that was never registered has nothing to remove; issuing the
        // DEL anyway could deregister a live registration of the same fd
        // (e.g. after `EEXIST` from a second `CoIo` over the same fd)
        if self.registered {
            del_socket(self);
        }
    }
}

impl AsFd for IoData {
    fn as_fd(&self) -> BorrowedFd<'_> {
        unsafe { BorrowedFd::borrow_raw(self.event_data.fd) }
    }
}

unsafe impl Send for IoData {}
