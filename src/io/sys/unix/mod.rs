extern crate nix;
extern crate libc;

#[cfg(target_os = "linux")]
#[path = "epoll.rs"]
mod select;

pub mod net;

use std::{io, fmt, ptr};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use sync::BoxedOption;
use scheduler::get_scheduler;
use coroutine::CoroutineImpl;
use timeout_list::TimeoutHandle;
use yield_now::{get_co_para, set_co_para};

pub use self::select::{SysEvent, Selector};

#[inline]
pub fn from_nix_error(err: nix::Error) -> ::std::io::Error {
    ::std::io::Error::from_raw_os_error(err.errno() as i32)
}

pub fn timeout_handler(data: TimerData) {
    if data.event_data.is_null() {
        return;
    }

    let event_data = unsafe { &mut *data.event_data };
    // remove the event timer
    event_data.timer.take();

    // get and check the coroutine
    let co = event_data.co.take(Ordering::Relaxed);
    if co.is_none() {
        // there is no coroutine, just ignore this one
        error!("can't get coroutine in timeout handler");
        return;
    }

    let mut co = co.unwrap();
    set_co_para(&mut co, io::Error::new(io::ErrorKind::TimedOut, "timeout"));
    get_scheduler().schedule(co);
}

type TimerHandle = TimeoutHandle<TimerData>;

// the timeout data
pub struct TimerData {
    event_data: *mut EventData,
}

// event associated io data, must be construct in
// each file handle, the epoll event.data would point to it
pub struct EventData {
    pub fd: RawFd,
    pub io_flag: AtomicBool,
    pub timer: Option<TimerHandle>,
    pub co: BoxedOption<CoroutineImpl>,
}

impl EventData {
    pub fn new(fd: RawFd) -> EventData {
        EventData {
            fd: fd,
            io_flag: AtomicBool::new(false),
            timer: None,
            co: BoxedOption::none(),
        }
    }

    pub fn timer_data(&self) -> TimerData {
        TimerData { event_data: self as *const _ as *mut _ }
    }

    #[inline]
    pub fn schedule(&mut self) {
        info!("event schedul");
        let co = self.co.take(Ordering::Acquire);
        if co.is_none() {
            // it's alreay take by selector
            return;
        }
        let mut co = co.unwrap();
        // co.prefetch();
        // it's safe to remove the timer since we are runing the timer_list in the same thread
        // self.timer.take().map(|h| {
        //     unsafe {
        //         // tell the timer function not to cancel the io
        //         // it's not always true that you can really remove the timer entry
        //         h.get_data().data.event_data = ptr::null_mut();
        //     }
        //     h.remove()
        // });

        // schedule the coroutine
        match co.resume() {
            Some(ev) => ev.subscribe(co),
            None => panic!("coroutine not return!"),
        }
    }
}

// each file associated data
#[derive(Clone, Copy)]
pub struct IoData(*mut EventData);

impl IoData {
    pub fn new<T: AsRawFd + ?Sized>(t: &T) -> Self {
        let fd = t.as_raw_fd();
        let event_data = Box::new(EventData::new(fd));
        IoData(Box::into_raw(event_data))
    }

    #[inline]
    pub fn inner(&self) -> &mut EventData {
        unsafe { &mut *self.0 }
    }

    // clear the io flag
    #[inline]
    pub fn reset(&self) {
        self.inner().io_flag.store(false, Ordering::Relaxed);
    }
}

impl fmt::Debug for IoData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, " IoData = {{ ... }}")
    }
}

unsafe impl Send for IoData {}

#[inline]
pub fn add_socket<T: AsRawFd + ?Sized>(t: &T) -> io::Result<IoData> {
    let io_data = IoData::new(t);
    let s = get_scheduler().get_selector();
    match s.add_fd(io_data.inner()) {
        Ok(_) => return Ok(io_data),
        Err(e) => {
            unsafe { Box::from_raw(io_data.0) };
            return Err(e);
        }
    }
}

#[inline]
pub fn del_socket(io: &IoData) {
    let s = get_scheduler().get_selector();
    s.del_fd(io.inner()).unwrap_or_else(|e| error!("failed to del_fd err={}:?", e));
}

// deal with the io result
#[inline]
fn co_io_result() -> io::Result<()> {
    match get_co_para() {
        Some(err) => {
            return Err(err);
        }
        None => {
            return Ok(());
        }
    }
}
