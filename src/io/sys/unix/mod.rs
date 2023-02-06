#[cfg(any(target_os = "linux", target_os = "android"))]
#[path = "epoll.rs"]
mod select;

#[cfg(any(
    target_os = "bitrig",
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

#[cfg(feature = "io_timeout")]
use std::cell::RefCell;
use std::ops::Deref;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
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

pub use self::select::{Selector, SysEvent};

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

    let event_data = unsafe { &mut *data.event_data };
    // remove the event timer
    event_data.timer.borrow_mut().take();

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
pub type TimerList = TimeOutList<TimerData>;
#[cfg(feature = "io_timeout")]
pub type TimerHandle = TimeoutHandle<TimerData>;

// event associated io data, must be construct in
// each file handle, the epoll event.data would point to it
pub struct EventData {
    pub fd: RawFd,
    pub io_flag: AtomicBool,
    #[cfg(feature = "io_timeout")]
    pub timer: RefCell<Option<TimerHandle>>,
    pub co: AtomicOption<CoroutineImpl>,
}

unsafe impl Send for EventData {}
unsafe impl Sync for EventData {}

impl EventData {
    pub fn new(fd: RawFd) -> EventData {
        EventData {
            fd,
            io_flag: AtomicBool::new(false),
            #[cfg(feature = "io_timeout")]
            timer: RefCell::new(None),
            co: AtomicOption::none(),
        }
    }

    #[cfg(feature = "io_timeout")]
    pub fn timer_data(&self) -> TimerData {
        TimerData {
            event_data: self as *const _ as *mut _,
        }
    }

    #[inline]
    pub fn schedule(&self) {
        info!("event schedule");
        let co = match self.co.take() {
            None => return, // it's already take by selector
            Some(co) => co,
        };

        // it's safe to remove the timer since we are running the timer_list in the same thread
        #[cfg(feature = "io_timeout")]
        self.timer.borrow_mut().take().map(|h| {
            unsafe {
                // tell the timer function not to cancel the io
                // it's not always true that you can really remove the timer entry
                h.with_mut_data(|value| value.data.event_data = std::ptr::null_mut());
            }
            h.remove()
        });

        // schedule the coroutine
        get_scheduler().schedule(co);
    }

    /// used by local re-schedule that in `subscribe`
    #[inline]
    pub fn fast_schedule(&self) {
        info!("event fast_schedule");

        let co = match self.co.take() {
            None => return, // it's already take by selector
            Some(co) => co,
        };

        // it's safe to remove the timer since we are running the timer_list in the same thread
        #[cfg(feature = "io_timeout")]
        self.timer.borrow_mut().take().map(|h| {
            unsafe {
                // tell the timer function not to cancel the io
                // it's not always true that you can really remove the timer entry
                h.with_mut_data(|value| value.data.event_data = std::ptr::null_mut());
            }
            h.remove()
        });

        // run the coroutine
        run_coroutine(co);
    }
}

// each file associated data
pub struct IoData(Arc<EventData>);

impl IoData {
    pub fn new<T: AsRawFd + ?Sized>(t: &T) -> Self {
        let fd = t.as_raw_fd();
        let event_data = Arc::new(EventData::new(fd));
        IoData(event_data)
    }

    // clear the io flag
    #[inline]
    pub fn reset(&self) {
        self.io_flag.store(false, Ordering::Relaxed);
    }
}

impl Deref for IoData {
    type Target = Arc<EventData>;

    fn deref(&self) -> &Arc<EventData> {
        &self.0
    }
}

impl fmt::Debug for IoData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "IoData = {{ ... }}")
    }
}

impl Drop for IoData {
    fn drop(&mut self) {
        del_socket(self);
    }
}

unsafe impl Send for IoData {}
