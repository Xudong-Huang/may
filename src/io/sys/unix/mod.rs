extern crate nix;
extern crate libc;

#[cfg(target_os = "linux")]
#[path = "epoll.rs"]
mod select;

pub mod net;

use std::io;
use std::os::unix::io::RawFd;
use yield_now::set_co_para;
use scheduler::get_scheduler;
use coroutine::CoroutineImpl;
use timeout_list::TimeoutHandle;

pub use self::select::{SysEvent, Selector};

bitflags! {
    flags EventFlags: u32 {
        const FLAG_READ  = 0b00000001,
        const FLAG_WRITE = 0b00000010,
    }
}

#[inline]
pub fn from_nix_error(err: nix::Error) -> ::std::io::Error {
    ::std::io::Error::from_raw_os_error(err.errno() as i32)
}

// when timeout happend we need to cancel the io operation
// this will trigger an event on the IOCP and processed in the selector
pub fn timeout_handler(data: TimerData) {
    if data.event_data.is_null() {
        return;
    }

    let event_data = unsafe { &mut *data.event_data };
    // remove the event timer
    event_data.timer.take();

    let s = get_scheduler();
    // ignore the error, the select may grab the data first!
    s.get_selector().cancel_io(event_data).map_err(|e| error!("cancel io failed! e = {}", e)).ok();

    let co = event_data.co.take();
    if co.is_none() {
        // there is no coroutine prepared, just ignore this one
        warn!("can't get coroutine in the epoll select");
        return;
    }

    let mut co = co.unwrap();
    set_co_para(&mut co, io::Error::new(io::ErrorKind::TimedOut, "timeout"));
    s.schedule_io(co);
}

type TimerHandle = TimeoutHandle<TimerData>;

// the timeout data
pub struct TimerData {
    event_data: *mut EventData,
}

// event associated io data, must be construct in the coroutine
// the timer handle is used to remove from the timeout list
// and co will be pushed to the event_list for scheduler
pub struct EventData {
    pub fd: RawFd,
    pub interest: EventFlags,
    pub timer: Option<TimerHandle>,
    pub co: Option<CoroutineImpl>,
}

impl EventData {
    pub fn new(fd: RawFd, interest: EventFlags) -> EventData {
        EventData {
            fd: fd,
            interest: interest,
            timer: None,
            co: None,
        }
    }

    pub fn timer_data(&self) -> TimerData {
        TimerData { event_data: self as *const _ as *mut _ }
    }
}
