extern crate nix;
extern crate libc;

#[cfg(any(target_os = "linux", target_os = "android"))]
#[path = "epoll.rs"]
mod select;

#[cfg(any(target_os = "bitrig", target_os = "dragonfly", target_os = "freebsd",
            target_os = "ios", target_os = "macos", target_os = "netbsd", target_os = "openbsd"))]
#[path = "kqueue.rs"]
mod select;

pub mod net;
pub mod cancel;

use std::sync::Arc;
use std::ops::Deref;
use std::cell::RefCell;
use std::{io, fmt, ptr};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use sync::AtomicOption;
use scheduler::get_scheduler;
use yield_now::{get_co_para, set_co_para};
use coroutine_impl::{CoroutineImpl, run_coroutine};
use timeout_list::{TimeoutHandle, TimeOutList};

pub use self::select::{SysEvent, Selector};

#[inline]
pub fn add_socket<T: AsRawFd + ?Sized>(t: &T) -> io::Result<IoData> {
    get_scheduler().get_selector().add_fd(IoData::new(t))
}

#[inline]
pub fn del_socket(io: &IoData) {
    // transfer the io to the selector
    get_scheduler().get_selector().del_fd(io.clone());
}

// deal with the io result
#[inline]
fn co_io_result() -> io::Result<()> {
    match get_co_para() {
        Some(err) => return Err(err),
        None => return Ok(()),
    }
}

#[inline]
fn from_nix_error(err: nix::Error) -> ::std::io::Error {
    use self::nix::Error::*;
    match err {
        Sys(errno) => ::std::io::Error::from_raw_os_error(errno as i32),
        InvalidPath => {
            ::std::io::Error::new(::std::io::ErrorKind::InvalidData, "nix invalide path")
        }
        InvalidUtf8 => {
            ::std::io::Error::new(::std::io::ErrorKind::InvalidData, "nix invalide utf8")
        }
        UnsupportedOperation => {
            ::std::io::Error::new(
                ::std::io::ErrorKind::PermissionDenied,
                "nix unsupported operation",
            )
        }
    }
}

fn timeout_handler(data: TimerData) {
    if data.event_data.is_null() {
        return;
    }

    let event_data = unsafe { &mut *data.event_data };
    // remove the event timer
    event_data.timer.borrow_mut().take();

    // get and check the coroutine
    let mut co = match event_data.co.take(Ordering::Relaxed) {
        Some(co) => co,
        None => return,
    };

    set_co_para(&mut co, io::Error::new(io::ErrorKind::TimedOut, "timeout"));

    // resume the coroutine with timeout error
    run_coroutine(co);
}


// the timeout data
pub struct TimerData {
    event_data: *mut EventData,
}

pub type TimerList = TimeOutList<TimerData>;
pub type TimerHandle = TimeoutHandle<TimerData>;

// event associated io data, must be construct in
// each file handle, the epoll event.data would point to it
pub struct EventData {
    pub fd: RawFd,
    pub io_flag: AtomicBool,
    pub timer: RefCell<Option<TimerHandle>>,
    pub co: AtomicOption<CoroutineImpl>,
}

unsafe impl Send for EventData {}
unsafe impl Sync for EventData {}

impl EventData {
    pub fn new(fd: RawFd) -> EventData {
        EventData {
            fd: fd,
            io_flag: AtomicBool::new(false),
            timer: RefCell::new(None),
            co: AtomicOption::none(),
        }
    }

    pub fn timer_data(&self) -> TimerData {
        TimerData { event_data: self as *const _ as *mut _ }
    }

    #[inline]
    pub fn schedule(&self) {
        info!("event schedul");
        let co = match self.co.take(Ordering::Acquire) {
            None => return, // it's already take by selector
            Some(co) => co,
        };

        // it's safe to remove the timer since we are running the timer_list in the same thread
        self.timer.borrow_mut().take().map(|h| {
            unsafe {
                // tell the timer function not to cancel the io
                // it's not always true that you can really remove the timer entry
                h.get_data().data.event_data = ptr::null_mut();
            }
            h.remove()
        });

        // schedule the coroutine
        run_coroutine(co);
    }
}

// each file associated data
#[derive(Clone)]
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

unsafe impl Send for IoData {}
